//! WASAPI shared-mode capture.
//!
//! Event-driven shared-mode capture: the thread sleeps in the kernel until
//! the audio engine signals a period, then drains all pending packets. Each
//! packet carries a QPC timestamp (100 ns units) — the same clock domain as
//! the video frames, which is what the muxer uses to keep the two in sync.

use std::{
    io::Write,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use wasapi::{
    AudioCaptureClient, AudioClient, DeviceEnumerator, Direction, Handle, SampleType, StreamMode,
    WaveFormat,
};

use super::{
    AudioGains, AudioPacket, CHANNELS, SAMPLE_RATE,
    devices::{AudioApartment, capture_device_by_id},
};

/// Which device a capture session reads.
///
/// Both are shared-mode *capture* clients — the difference is only which
/// default device is enumerated. Initializing a capture client against a
/// **render** device is what makes it a loopback stream; against a **capture**
/// device it is an ordinary microphone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSourceKind {
    SystemAudio,
    Microphone,
}

impl AudioSourceKind {
    fn direction(self) -> Direction {
        match self {
            Self::SystemAudio => Direction::Render,
            Self::Microphone => Direction::Capture,
        }
    }

    /// User-facing name, used in the warning shown when a source is missing.
    pub fn label(self) -> &'static str {
        match self {
            Self::SystemAudio => "system audio",
            Self::Microphone => "microphone",
        }
    }
}

/// An event-driven shared-mode capture session on whichever default device
/// [`AudioSourceKind::direction`] resolves to. "Loopback" describes only the
/// system-audio case (a capture client initialized against the render
/// device); against a capture device this is an ordinary microphone stream.
struct WasapiSession {
    client: AudioClient,
    event: Handle,
    capture: AudioCaptureClient,
    block_align: usize,
    scratch: Vec<u8>,
}

impl WasapiSession {
    fn open(kind: AudioSourceKind, format: &WaveFormat, device_id: Option<&str>) -> Result<Self> {
        let enumerator = DeviceEnumerator::new().map_err(|e| anyhow!("device enumerator: {e}"))?;
        let device = match device_id {
            Some(id) => capture_device_by_id(&enumerator, id)?,
            None => enumerator
                .get_default_device(&kind.direction())
                .map_err(|e| anyhow!("no default {} device: {e}", kind.label()))?,
        };
        let mut client = device.get_iaudioclient().map_err(|e| anyhow!("audio client: {e}"))?;
        client
            .initialize_client(
                format,
                &Direction::Capture,
                // 200 ms device buffer: ample slack, engine period stays ~10 ms.
                &StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 2_000_000 },
            )
            .map_err(|e| anyhow!("{} init failed: {e}", kind.label()))?;
        let event = client.set_get_eventhandle().map_err(|e| anyhow!("event handle: {e}"))?;
        let capture =
            client.get_audiocaptureclient().map_err(|e| anyhow!("capture client: {e}"))?;
        let block_align = format.get_blockalign() as usize;
        let buffer_frames = client.get_buffer_size().map_err(|e| anyhow!("buffer size: {e}"))?;
        let scratch = vec![0u8; (buffer_frames as usize).max(4096) * block_align];
        Ok(Self { client, event, capture, block_align, scratch })
    }
}

/// The device boundary: recovery owns the channel and meter independently of
/// any one WASAPI client. Tests can disconnect a stream without touching the
/// user's Windows devices.
trait CaptureStream {
    fn start(&mut self) -> Result<()>;
    fn next_packet(&mut self) -> Result<Option<AudioPacket>>;
    fn stop(&mut self) -> Result<()>;
}

impl CaptureStream for WasapiSession {
    fn start(&mut self) -> Result<()> {
        self.client.start_stream().map_err(|e| anyhow!("start stream: {e}"))
    }

    fn next_packet(&mut self) -> Result<Option<AudioPacket>> {
        let mut frames = self.capture.get_next_packet_size()?;
        if frames.unwrap_or(0) == 0 {
            // Silence is normal. Still query WASAPI after a timeout: an
            // unplugged device may never signal this event again, and only
            // the query exposes AUDCLNT_E_DEVICE_INVALIDATED in that case.
            let _ = self.event.wait_for_event(100);
            frames = self.capture.get_next_packet_size()?;
        }
        if frames.unwrap_or(0) == 0 {
            return Ok(None);
        }
        let (read_frames, info) = self.capture.read_from_device(&mut self.scratch)?;
        if read_frames == 0 {
            return Ok(None);
        }
        let pcm = &mut self.scratch[..read_frames as usize * self.block_align];
        if info.flags.silent {
            pcm.fill(0);
        }
        Ok(Some(AudioPacket { qpc_100ns: info.timestamp as i64, data: pcm.to_vec() }))
    }

    fn stop(&mut self) -> Result<()> {
        self.client.stop_stream().map_err(|e| anyhow!("stop stream: {e}"))
    }
}

/// Background capture thread feeding [`AudioPacket`]s to a channel.
pub struct AudioCapture {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<Result<()>>,
}

impl AudioCapture {
    pub fn start(kind: AudioSourceKind) -> Result<(Self, Receiver<AudioPacket>)> {
        Self::start_selected(kind, None, None)
    }

    pub(super) fn start_selected(
        kind: AudioSourceKind,
        device_id: Option<String>,
        gains: Option<Arc<AudioGains>>,
    ) -> Result<(Self, Receiver<AudioPacket>)> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let stop_flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name(match kind {
                AudioSourceKind::SystemAudio => "trix-audio".into(),
                AudioSourceKind::Microphone => "trix-mic".into(),
            })
            .spawn(move || {
                capture_loop(kind, device_id.as_deref(), gains.as_deref(), &stop_flag, &tx)
            })
            .context("failed to spawn audio thread")?;
        Ok((Self { stop, thread }, rx))
    }

    pub fn stop(self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        // A missing device sleeps between attempts; disarming must not wait
        // for that retry delay to expire.
        self.thread.thread().unpark();
        match self.thread.join() {
            Ok(result) => result,
            Err(_) => bail!("audio capture thread panicked"),
        }
    }
}

fn capture_loop(
    kind: AudioSourceKind,
    device_id: Option<&str>,
    gains: Option<&AudioGains>,
    stop: &AtomicBool,
    tx: &Sender<AudioPacket>,
) -> Result<()> {
    let _apartment = AudioApartment::enter()?;
    // i16 directly: the audio engine autoconverts its float mix (and a mono
    // or 44.1 kHz microphone), so packets are already in the encoder's format.
    let format = WaveFormat::new(16, 16, &SampleType::Int, SAMPLE_RATE, CHANNELS, None);
    capture_with_recovery(
        kind,
        gains,
        stop,
        tx,
        // Resolve Windows default again on each attempt. An explicit ID stays
        // fixed, so losing a chosen mic never opens a different microphone.
        || WasapiSession::open(kind, &format, device_id),
        || wait_to_retry(stop),
    )
}

fn wait_to_retry(stop: &AtomicBool) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while !stop.load(Ordering::Acquire) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        std::thread::park_timeout(remaining);
    }
}

fn capture_with_recovery<S: CaptureStream>(
    kind: AudioSourceKind,
    gains: Option<&AudioGains>,
    stop: &AtomicBool,
    tx: &Sender<AudioPacket>,
    mut open: impl FnMut() -> Result<S>,
    mut wait: impl FnMut(),
) -> Result<()> {
    // Clear availability on every exit, including a panic during capture.
    struct MeterGuard<'a>(Option<&'a AudioGains>);
    impl Drop for MeterGuard<'_> {
        fn drop(&mut self) {
            if let Some(gains) = self.0 {
                gains.input_meter.set_active(false);
            }
        }
    }
    let _meter = MeterGuard(gains);
    let mut warned = false;
    while !stop.load(Ordering::Acquire) {
        let result: Result<()> = (|| {
            let mut session = open()?;
            // Stop might arrive during device initialization.
            if stop.load(Ordering::Acquire) {
                return Ok(());
            }
            session.start()?;
            if let Some(gains) = gains {
                gains.input_meter.set_active(true);
            }
            let captured = (|| {
                while !stop.load(Ordering::Acquire) {
                    let Some(packet) = session.next_packet()? else { continue };
                    if let Some(gains) = gains {
                        gains.input_meter.observe(&packet.data, gains.mic());
                    }
                    if tx.send(packet).is_err() {
                        break; // consumer gone — recording ended, do not reopen
                    }
                }
                Ok(())
            })();
            // A removed device can also reject Stop. Release every client
            // before attempting to reopen; the read error is the useful one.
            let _ = session.stop();
            captured
        })();
        if let Some(gains) = gains {
            gains.input_meter.set_active(false);
        }
        match result {
            Ok(()) => return Ok(()),
            Err(_) if stop.load(Ordering::Acquire) => return Ok(()),
            Err(error) => {
                if !warned {
                    tracing::warn!(
                        error = %format!("{error:#}"),
                        "{} unavailable; recording continues, retrying automatically",
                        kind.label()
                    );
                    warned = true;
                }
            }
        }
        // Only the existing audio thread waits. The mixer keeps its channel
        // and QPC timeline, filling the gap without resetting the replay ring.
        wait();
    }
    Ok(())
}

/// Captures `seconds` of system loopback audio and writes it as a
/// 32-bit-float WAV, printing capture-quality statistics (probe mode).
pub fn record_wav(seconds: u64, path: &Path) -> Result<()> {
    wasapi::initialize_mta().ok().context("COM MTA init failed")?;
    println!("capturing {seconds} s of system audio — play some sound to get a signal");

    // f32 stereo 48 kHz with autoconvert: the engine resamples whatever the
    // device mix format is, so downstream code sees exactly one format.
    let format = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE, CHANNELS, None);
    let mut session = WasapiSession::open(AudioSourceKind::SystemAudio, &format, None)?;
    let block_align = session.block_align;
    let mut pcm: Vec<u8> = Vec::with_capacity(SAMPLE_RATE * block_align * seconds as usize);

    let mut packets = 0u64;
    let mut silent_packets = 0u64;
    let mut discontinuities = 0u64;
    let mut first_qpc_100ns: Option<u64> = None;
    let mut last_qpc_100ns = 0u64;

    session.client.start_stream().map_err(|e| anyhow!("start stream: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(seconds);

    while Instant::now() < deadline {
        // Loopback delivers packets only while something renders; timing out
        // just means system-wide silence, so keep waiting until the deadline.
        if session.event.wait_for_event(200).is_err() {
            continue;
        }
        while let Ok(Some(frames)) = session.capture.get_next_packet_size() {
            if frames == 0 {
                break;
            }
            let (read_frames, info) = session
                .capture
                .read_from_device(&mut session.scratch)
                .map_err(|e| anyhow!("read from device: {e}"))?;
            if read_frames == 0 {
                break;
            }
            packets += 1;
            if info.flags.silent {
                silent_packets += 1;
            }
            if info.flags.data_discontinuity && packets > 1 {
                discontinuities += 1;
            }
            if first_qpc_100ns.is_none() {
                first_qpc_100ns = Some(info.timestamp);
            }
            last_qpc_100ns = info.timestamp;
            pcm.extend_from_slice(&session.scratch[..read_frames as usize * block_align]);
        }
    }
    session.client.stop_stream().map_err(|e| anyhow!("stop stream: {e}"))?;

    if pcm.is_empty() {
        bail!("no audio packets captured — was anything playing during the test?");
    }
    write_wav_f32(path, &pcm)?;

    let (peak, rms) = analyze_f32(&pcm);
    let captured_secs = pcm.len() as f64 / (SAMPLE_RATE * block_align) as f64;
    let qpc_span_secs =
        first_qpc_100ns.map(|f| (last_qpc_100ns - f) as f64 / 10_000_000.0).unwrap_or(0.0);

    println!("\n[audio capture]");
    println!("  written:         {} ({:.2} s of PCM)", path.display(), captured_secs);
    println!("  packets:         {packets} ({silent_packets} silent)");
    println!("  discontinuities: {discontinuities}");
    println!("  qpc span:        {qpc_span_secs:.2} s");
    println!("  peak:            {:.1} dBFS", 20.0 * peak.max(1e-9).log10());
    println!("  rms:             {:.1} dBFS", 20.0 * rms.max(1e-9).log10());
    if discontinuities > 0 {
        println!("  WARNING: gaps detected — buffer overrun or device stall");
    }
    Ok(())
}

fn analyze_f32(pcm: &[u8]) -> (f32, f32) {
    let mut peak = 0.0f32;
    let mut sum_sq = 0.0f64;
    let samples = pcm.len() / 4;
    for chunk in pcm.chunks_exact(4) {
        let s = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        peak = peak.max(s.abs());
        sum_sq += f64::from(s) * f64::from(s);
    }
    (peak, (sum_sq / samples.max(1) as f64).sqrt() as f32)
}

/// Minimal RIFF/WAVE writer for 32-bit IEEE-float stereo PCM.
fn write_wav_f32(path: &Path, pcm: &[u8]) -> Result<()> {
    let byte_rate = (SAMPLE_RATE * CHANNELS * 4) as u32;
    let block_align = (CHANNELS * 4) as u16;
    let data_len = pcm.len() as u32;

    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_len).to_le_bytes())?;
    file.write_all(b"WAVE")?;
    file.write_all(b"fmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&3u16.to_le_bytes())?; // WAVE_FORMAT_IEEE_FLOAT
    file.write_all(&(CHANNELS as u16).to_le_bytes())?;
    file.write_all(&(SAMPLE_RATE as u32).to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&32u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_len.to_le_bytes())?;
    file.write_all(pcm)?;
    file.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    use super::super::{AudioMixer, InputLevel};
    use super::*;

    enum Step {
        Packet(i64, i16),
        Idle,
        Disconnect,
        Stop,
    }

    /// Only the external device is scripted. Channel delivery, metering,
    /// recovery, and mixing all run through their production implementations.
    struct TestStream<'a> {
        steps: VecDeque<Step>,
        stop: &'a AtomicBool,
        gains: &'a AudioGains,
        levels: &'a RefCell<Vec<InputLevel>>,
        live: &'a Cell<bool>,
        fail_start: bool,
    }

    impl CaptureStream for TestStream<'_> {
        fn start(&mut self) -> Result<()> {
            assert!(!self.live.replace(true), "old client must be released before reopening");
            if self.fail_start {
                bail!("device disappeared while starting");
            }
            Ok(())
        }

        fn next_packet(&mut self) -> Result<Option<AudioPacket>> {
            let level = self.gains.input_level();
            assert!(level.active, "only a started stream is available");
            self.levels.borrow_mut().push(level);
            match self.steps.pop_front().expect("capture ran past the scripted device events") {
                Step::Packet(qpc_100ns, value) => Ok(Some(AudioPacket {
                    qpc_100ns,
                    data: [value, value].into_iter().flat_map(i16::to_le_bytes).collect(),
                })),
                Step::Idle => Ok(None),
                Step::Disconnect => bail!("audio device invalidated"),
                Step::Stop => {
                    self.stop.store(true, Ordering::Release);
                    Ok(None)
                }
            }
        }

        fn stop(&mut self) -> Result<()> {
            Ok(())
        }
    }

    impl Drop for TestStream<'_> {
        fn drop(&mut self) {
            self.live.set(false);
        }
    }

    #[test]
    fn each_source_kind_names_itself_for_the_log() {
        // These strings reach the user in a warning when a source cannot be
        // opened ("microphone unavailable, …"), so they are user-facing copy,
        // not debug output.
        assert_eq!(AudioSourceKind::SystemAudio.label(), "system audio");
        assert_eq!(AudioSourceKind::Microphone.label(), "microphone");
    }

    #[test]
    fn the_two_source_kinds_read_different_devices() {
        // The whole difference between recording the speakers and recording
        // the microphone is which default device is enumerated. If these ever
        // matched, Trix would silently record system audio twice.
        //
        // `matches!` rather than `assert_ne!`: `wasapi::Direction` is a
        // third-party enum and is not guaranteed to implement `PartialEq`.
        assert!(matches!(AudioSourceKind::SystemAudio.direction(), Direction::Render));
        assert!(matches!(AudioSourceKind::Microphone.direction(), Direction::Capture));
    }

    #[test]
    fn an_unavailable_microphone_keeps_waiting_until_capture_is_stopped() {
        let gains = AudioGains::new(0, 100);
        let (capture, rx) = AudioCapture::start_selected(
            AudioSourceKind::Microphone,
            Some("{trix-test-missing-microphone}".into()),
            Some(Arc::clone(&gains)),
        )
        .unwrap();
        let received = rx.recv_timeout(Duration::from_millis(200));
        let level = gains.input_level();
        let stopped_at = Instant::now();
        let stopped = capture.stop();
        assert!(
            matches!(received, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
            "an unavailable device must not permanently close its audio channel"
        );
        assert!(!level.active);
        stopped.expect("stopping while waiting for a microphone is normal");
        assert!(stopped_at.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn microphone_recovers_repeated_disconnects_on_the_original_audio_timeline() {
        let gains = AudioGains::new(0, 100);
        let stop = AtomicBool::new(false);
        let live = Cell::new(false);
        let levels = RefCell::new(Vec::new());
        let (tx, rx) = std::sync::mpsc::channel();
        let mut attempts = 0;
        let mut waits = 0;
        capture_with_recovery(
            AudioSourceKind::Microphone,
            Some(&gains),
            &stop,
            &tx,
            || {
                assert!(!live.get(), "reopening must release the disconnected client first");
                attempts += 1;
                let steps = match attempts {
                    1 => vec![Step::Packet(1_000_000, 1000), Step::Disconnect],
                    2 => bail!("microphone still unplugged"),
                    3 => vec![], // It returned, but disappears again during Start.
                    4 => vec![Step::Packet(2_000_000, 2000), Step::Disconnect],
                    5 => vec![Step::Packet(3_000_000, 3000), Step::Stop],
                    _ => panic!("capture reopened after Stop"),
                };
                Ok(TestStream {
                    steps: steps.into(),
                    stop: &stop,
                    gains: &gains,
                    levels: &levels,
                    live: &live,
                    fail_start: attempts == 3,
                })
            },
            || {
                waits += 1;
                let level = gains.input_level();
                assert!(!level.active, "the meter must report unavailable during retries");
                assert_eq!(level.peak, 0.0, "a disconnected mic must not show a stale peak");
                assert!(!live.get(), "release the device before waiting");
            },
        )
        .unwrap();
        assert_eq!(waits, 4, "each failed attempt must back off, including failed Start");
        assert!(!live.get());
        assert!(!gains.input_level().active);
        let peaks: Vec<_> = levels.borrow().iter().map(|level| level.peak).collect();
        assert_eq!(peaks, [0.0, 1000.0 / 32768.0, 0.0, 2000.0 / 32768.0, 0.0, 3000.0 / 32768.0]);

        // Reopened packets retain their QPC stamps and enter the same mixer.
        // The two 100 ms gaps become silence; recovered speech stays in sync.
        let mut mixer = AudioMixer::with_sources(None, Some(rx), gains);
        mixer.start(1_000_000);
        let mut pcm = Vec::new();
        mixer.pump(3_000_000, &mut |_, data| pcm.extend_from_slice(data));
        assert_eq!(mixer.frames_emitted(), 9601);
        let frames: Vec<_> =
            pcm.chunks_exact(4).map(|frame| i16::from_le_bytes([frame[0], frame[1]])).collect();
        assert_eq!(frames[0], 1000);
        assert_eq!(frames[4800], 2000);
        assert_eq!(frames[9600], 3000);
        assert!(frames[1..4800].iter().all(|sample| *sample == 0));
        assert!(frames[4801..9600].iter().all(|sample| *sample == 0));
    }

    #[test]
    fn quiet_microphone_stays_open_and_does_not_trigger_recovery() {
        let gains = AudioGains::new(0, 100);
        let stop = AtomicBool::new(false);
        let live = Cell::new(false);
        let levels = RefCell::new(Vec::new());
        let (tx, rx) = std::sync::mpsc::channel();
        let mut attempts = 0;
        capture_with_recovery(
            AudioSourceKind::Microphone,
            Some(&gains),
            &stop,
            &tx,
            || {
                attempts += 1;
                Ok(TestStream {
                    steps: [Step::Idle, Step::Idle, Step::Packet(123, 0), Step::Stop].into(),
                    stop: &stop,
                    gains: &gains,
                    levels: &levels,
                    live: &live,
                    fail_start: false,
                })
            },
            || panic!("silence must not restart a healthy microphone"),
        )
        .unwrap();
        assert_eq!(attempts, 1);
        assert_eq!(rx.try_recv().unwrap().data, [0; 4]);
        assert!(!gains.input_level().active);
        assert!(!live.get());
    }

    #[test]
    fn recording_consumer_gone_does_not_reopen_the_microphone() {
        let gains = AudioGains::new(0, 100);
        let stop = AtomicBool::new(false);
        let live = Cell::new(false);
        let levels = RefCell::new(Vec::new());
        let (tx, rx) = std::sync::mpsc::channel();
        drop(rx);
        let mut attempts = 0;
        capture_with_recovery(
            AudioSourceKind::Microphone,
            Some(&gains),
            &stop,
            &tx,
            || {
                attempts += 1;
                Ok(TestStream {
                    steps: [Step::Packet(123, 100)].into(),
                    stop: &stop,
                    gains: &gains,
                    levels: &levels,
                    live: &live,
                    fail_start: false,
                })
            },
            || panic!("an ended recording must not retry"),
        )
        .unwrap();
        assert_eq!(attempts, 1);
        assert!(!gains.input_level().active);
        assert!(!live.get());
    }

    #[test]
    #[ignore = "briefly reopens the real Windows-default microphone; saves no audio"]
    fn default_microphone_reopens_after_a_stream_failure() {
        struct InterruptedStream<'a> {
            session: WasapiSession,
            stop: &'a AtomicBool,
            delivered: bool,
            recovered: bool,
            deadline: Instant,
        }
        impl CaptureStream for InterruptedStream<'_> {
            fn start(&mut self) -> Result<()> {
                self.session.start()
            }
            fn stop(&mut self) -> Result<()> {
                self.session.stop()
            }
            fn next_packet(&mut self) -> Result<Option<AudioPacket>> {
                if Instant::now() >= self.deadline {
                    self.stop.store(true, Ordering::Release);
                    bail!("microphone supplied no PCM within the hardware-check budget");
                }
                if self.delivered {
                    if self.recovered {
                        self.stop.store(true, Ordering::Release);
                        return Ok(None);
                    }
                    bail!("injected stream invalidation after real microphone PCM");
                }
                let packet = self.session.next_packet()?;
                self.delivered = packet.is_some();
                Ok(packet)
            }
        }

        let _apartment = AudioApartment::enter().unwrap();
        let format = WaveFormat::new(16, 16, &SampleType::Int, SAMPLE_RATE, CHANNELS, None);
        let gains = AudioGains::new(0, 100);
        let stop = AtomicBool::new(false);
        let (tx, rx) = std::sync::mpsc::channel();
        let mut attempts = 0;
        capture_with_recovery(
            AudioSourceKind::Microphone,
            Some(&gains),
            &stop,
            &tx,
            || {
                attempts += 1;
                if attempts > 2 {
                    stop.store(true, Ordering::Release);
                    bail!("default microphone could not be reopened");
                }
                Ok(InterruptedStream {
                    session: WasapiSession::open(AudioSourceKind::Microphone, &format, None)?,
                    stop: &stop,
                    delivered: false,
                    recovered: attempts == 2,
                    deadline: Instant::now() + Duration::from_secs(2),
                })
            },
            || wait_to_retry(&stop),
        )
        .unwrap();
        let packets: Vec<_> = rx.try_iter().collect();
        assert_eq!(packets.len(), 2, "real PCM must arrive before and after reopening");
        assert!(packets.iter().all(|packet| !packet.data.is_empty()));
        assert!(packets[1].qpc_100ns > packets[0].qpc_100ns);
        assert!(!gains.input_level().active);
    }

    #[test]
    #[ignore = "opens a real microphone briefly; requires an available capture device"]
    fn selected_microphone_stream_updates_meter_and_closes() {
        let inputs = super::super::input_devices().expect("enumerate capture devices");
        let device = inputs.first().expect("no microphone is available for this hardware check");
        let gains = AudioGains::new(0, 100);
        gains.input_level();
        let (capture, rx) = AudioCapture::start_selected(
            AudioSourceKind::Microphone,
            Some(device.id.clone()),
            Some(Arc::clone(&gains)),
        )
        .expect("start selected microphone");
        let packet = rx.recv_timeout(Duration::from_secs(2));
        let level = gains.input_level();
        // Join even when the receive failed, so the check cannot leave a mic open.
        let stopped = capture.stop();
        stopped.expect("stop selected microphone");
        assert!(!packet.expect("selected microphone supplies PCM").data.is_empty());
        assert!(level.active);
        assert!(level.peak.is_finite() && (0.0..=1.0).contains(&level.peak));
        assert!(!gains.input_level().active, "stopping capture clears availability");
    }
}
