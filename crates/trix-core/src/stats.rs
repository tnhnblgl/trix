//! Phase 7 — performance self-certification: process memory with the
//! CPU-vs-GPU split, and fixed-bucket latency histograms.
//!
//! On UMA iGPUs (this machine's Quick Sync path) GPU surfaces are charged to
//! the process working set, so raw WS wildly overstates what the recorder
//! costs the *CPU side* of the machine. `MemoryReport` pairs the OS process
//! counters with DXGI's per-process video-memory usage so logs can show both
//! halves; `cpu_estimate` is WS minus GPU-attributed bytes (an estimate: on
//! discrete GPUs local VRAM never sits in WS, making the estimate
//! conservative there).

use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Graphics::Dxgi::{
    DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL,
    DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter3, IDXGIDevice,
};
use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::core::Interface;

/// Histogram bucket upper bounds in microseconds. The last bucket is
/// open-ended; 33 ms ≈ two frame intervals at 60 fps.
const BOUNDS_US: [u64; 9] = [500, 1_000, 2_000, 4_000, 8_000, 16_000, 33_000, 66_000, u64::MAX];

/// Fixed-size latency histogram: no allocation, O(1) record, quantiles read
/// as "≤ bucket bound". Good enough to certify budgets, cheap enough to sit
/// on the capture hot path.
#[derive(Default)]
pub struct LatencyHistogram {
    buckets: [u64; BOUNDS_US.len()],
    count: u64,
    max_us: u64,
}

impl LatencyHistogram {
    pub const fn new() -> Self {
        Self { buckets: [0; BOUNDS_US.len()], count: 0, max_us: 0 }
    }

    pub fn record(&mut self, elapsed: Duration) {
        let us = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
        let idx = BOUNDS_US.iter().position(|&b| us <= b).unwrap_or(BOUNDS_US.len() - 1);
        self.buckets[idx] += 1;
        self.count += 1;
        self.max_us = self.max_us.max(us);
    }

    /// Upper bound (µs) of the bucket where the cumulative count reaches the
    /// quantile. The open-ended top bucket reports the exact observed max.
    fn quantile_bound_us(&self, q: f64) -> u64 {
        let threshold = (q * self.count as f64).ceil().max(1.0) as u64;
        let mut cumulative = 0;
        for (bucket, bound) in self.buckets.iter().zip(BOUNDS_US) {
            cumulative += bucket;
            if cumulative >= threshold {
                return if bound == u64::MAX { self.max_us } else { bound };
            }
        }
        self.max_us
    }

    /// One-line summary for logs, e.g. `n=1234 p50<=2ms p99<=8ms max=6.8ms`.
    pub fn summary(&self) -> String {
        if self.count == 0 {
            return "n=0".into();
        }
        format!(
            "n={} p50<={} p99<={} max={}",
            self.count,
            fmt_us(self.quantile_bound_us(0.50)),
            fmt_us(self.quantile_bound_us(0.99)),
            fmt_us(self.max_us),
        )
    }
}

fn fmt_us(us: u64) -> String {
    if us < 1_000 { format!("{us}us") } else { format!("{:.1}ms", us as f64 / 1_000.0) }
}

/// Point-in-time memory snapshot, all fields in bytes.
pub struct MemoryReport {
    pub working_set: u64,
    /// This process's usage of the adapter's local segment (dedicated VRAM,
    /// or the carve-out on UMA).
    pub gpu_local: u64,
    /// This process's usage of the non-local segment (shared system memory —
    /// the part UMA charges to our working set).
    pub gpu_shared: u64,
}

impl MemoryReport {
    /// Working set minus the GPU memory DXGI attributes to this process.
    ///
    /// This is an *upper bound* on CPU-side RAM, not the heap: on a UMA iGPU
    /// DXGI's in-process counters see only the dedicated (local) segment, so
    /// GPU-accessible *shared* system memory (WGC frame pool, NV12 staging,
    /// encoder surfaces) stays folded in here even though it is GPU-side. The
    /// exact CPU-side figure is the compressed ring the caller measures
    /// directly. On discrete GPUs, where local VRAM never enters the working
    /// set, this instead undercounts — either way it is a bound, not truth.
    pub fn ws_minus_gpu(&self) -> u64 {
        self.working_set.saturating_sub(self.gpu_local + self.gpu_shared)
    }
}

/// Rounds bytes to tenths of a MiB for log fields.
pub fn mb(bytes: u64) -> f64 {
    (bytes as f64 / (1024.0 * 1024.0) * 10.0).round() / 10.0
}

/// This process's working set, in bytes.
///
/// Split out of [`StatsReporter::memory`] because it is the one memory figure
/// that needs no D3D device: `K32GetProcessMemoryInfo` asks the OS about the
/// current process and nothing else. That matters for the daemon's `stats`
/// event, which is emitted whether or not a capture session exists — and the
/// daemon has no device to build a [`StatsReporter`] around even when it is
/// armed, since the reporter lives *inside* the capture session. Creating a
/// second DXGI device in the daemon purely so it could report on itself would
/// cost the memory this project exists to save, so the daemon reports the
/// working set and no GPU numbers. `memory()` calls straight through to here,
/// so there is exactly one implementation of the call.
///
/// Returns 0 when the query fails rather than propagating: a stats line is a
/// diagnostic, and a missing number must not be able to end a capture. (The
/// single-pass version returned `WorkingSetSize` from a zeroed struct on that
/// path, which is the same 0 — the behaviour is preserved, not invented.)
pub fn working_set() -> u64 {
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    if !ok.as_bool() {
        tracing::warn!("process memory counters query failed");
        return 0;
    }
    counters.WorkingSetSize as u64
}

/// Owns the DXGI adapter handle for per-process video-memory queries and the
/// periodic-report timer. Lives inside a capture session.
pub struct StatsReporter {
    adapter: Option<IDXGIAdapter3>,
    interval: Option<Duration>,
    next_at: Instant,
}

// SAFETY: same contract as the session that owns this — moved to the capture
// thread and used from one thread at a time (ticks go through the handler
// mutex); DXGI adapter interfaces are free-threaded.
unsafe impl Send for StatsReporter {}

impl StatsReporter {
    /// `interval_secs == 0` disables the periodic report (final summaries
    /// still work). A device whose adapter lacks `IDXGIAdapter3` (pre-Win10)
    /// degrades to CPU-side counters only.
    pub fn new(device: &ID3D11Device, interval_secs: u32) -> Self {
        let adapter = (|| -> windows::core::Result<IDXGIAdapter3> {
            let dxgi: IDXGIDevice = device.cast()?;
            unsafe { dxgi.GetAdapter() }?.cast()
        })();
        let adapter = match adapter {
            Ok(adapter) => Some(adapter),
            Err(e) => {
                tracing::debug!("per-process GPU memory counters unavailable: {e}");
                None
            }
        };
        let interval = (interval_secs > 0).then(|| Duration::from_secs(u64::from(interval_secs)));
        Self { adapter, interval, next_at: Instant::now() + interval.unwrap_or(Duration::ZERO) }
    }

    /// True at most once per interval; always false when disabled.
    pub fn due(&mut self) -> bool {
        let Some(interval) = self.interval else { return false };
        if Instant::now() < self.next_at {
            return false;
        }
        self.next_at = Instant::now() + interval;
        true
    }

    pub fn memory(&self) -> MemoryReport {
        let query = |group| -> u64 {
            let Some(adapter) = &self.adapter else { return 0 };
            let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
            match unsafe { adapter.QueryVideoMemoryInfo(0, group, &mut info) } {
                Ok(()) => info.CurrentUsage,
                Err(_) => 0,
            }
        };

        MemoryReport {
            working_set: working_set(),
            gpu_local: query(DXGI_MEMORY_SEGMENT_GROUP_LOCAL),
            gpu_shared: query(DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_histogram_reports_n0() {
        assert_eq!(LatencyHistogram::new().summary(), "n=0");
    }

    #[test]
    fn quantiles_land_in_right_buckets() {
        let mut h = LatencyHistogram::new();
        for _ in 0..98 {
            h.record(Duration::from_micros(1_500)); // bucket <=2ms
        }
        h.record(Duration::from_micros(7_000)); // bucket <=8ms
        h.record(Duration::from_micros(40_000)); // bucket <=66ms
        assert_eq!(h.quantile_bound_us(0.50), 2_000);
        assert_eq!(h.quantile_bound_us(0.99), 8_000);
        assert_eq!(h.max_us, 40_000);
        assert_eq!(h.summary(), "n=100 p50<=2.0ms p99<=8.0ms max=40.0ms");
    }

    #[test]
    fn top_bucket_reports_observed_max() {
        let mut h = LatencyHistogram::new();
        h.record(Duration::from_millis(200));
        assert_eq!(h.quantile_bound_us(0.5), 200_000);
        assert_eq!(h.summary(), "n=1 p50<=200.0ms p99<=200.0ms max=200.0ms");
    }

    #[test]
    fn ws_minus_gpu_subtracts_both_segments_and_saturates() {
        let report =
            MemoryReport { working_set: 100 << 20, gpu_local: 30 << 20, gpu_shared: 50 << 20 };
        assert_eq!(report.ws_minus_gpu(), 20 << 20);
        let inverted = MemoryReport { gpu_shared: 90 << 20, ..report };
        assert_eq!(inverted.ws_minus_gpu(), 0);
    }

    #[test]
    fn mb_rounds_to_tenths() {
        assert_eq!(mb(136 << 20), 136.0);
        assert_eq!(mb((136 << 20) + 550 * 1024), 136.5);
    }

    // Exercises the real OS counters; no D3D device needed for the CPU half.
    #[test]
    fn process_counters_are_live() {
        let reporter = StatsReporter { adapter: None, interval: None, next_at: Instant::now() };
        let m = reporter.memory();
        assert!(m.working_set > 1 << 20, "working set should exceed 1 MiB");
        assert_eq!(m.gpu_local, 0);
        assert_eq!(m.ws_minus_gpu(), m.working_set);
    }

    /// The daemon's `stats` event calls this with no device anywhere in sight,
    /// so it has to work standing alone — and it has to be the *same* number
    /// `memory()` reports, or the daemon and the capture log would quote two
    /// different working sets for one process.
    #[test]
    fn working_set_is_live_without_a_device() {
        let standalone = working_set();
        assert!(standalone > 1 << 20, "working set should exceed 1 MiB");

        let reporter = StatsReporter { adapter: None, interval: None, next_at: Instant::now() };
        let through_the_reporter = reporter.memory().working_set;
        // Not `assert_eq!`: the two calls are microseconds apart and a real
        // working set moves. Within 64 MiB is "the same number", and still
        // fails loudly if one of them ever returns 0 or reads a different
        // counter.
        let drift = standalone.abs_diff(through_the_reporter);
        assert!(
            drift < 64 << 20,
            "working_set() and memory().working_set should be the same counter: \
             {standalone} vs {through_the_reporter}"
        );
    }
}
