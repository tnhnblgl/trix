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

use super::{AudioPacket, CHANNELS, SAMPLE_RATE};

/// An event-driven shared-mode loopback session on the default render device.
struct LoopbackSession {
    client: AudioClient,
    event: Handle,
    capture: AudioCaptureClient,
    block_align: usize,
    scratch: Vec<u8>,
}

impl LoopbackSession {
    fn open(format: &WaveFormat) -> Result<Self> {
        let enumerator = DeviceEnumerator::new().map_err(|e| anyhow!("device enumerator: {e}"))?;
        let device = enumerator
            .get_default_device(&Direction::Render)
            .map_err(|e| anyhow!("no default render device: {e}"))?;
        let mut client = device.get_iaudioclient().map_err(|e| anyhow!("audio client: {e}"))?;
        client
            .initialize_client(
                format,
                &Direction::Capture,
                // 200 ms device buffer: ample slack, engine period stays ~10 ms.
                &StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 2_000_000 },
            )
            .map_err(|e| anyhow!("loopback init failed: {e}"))?;
        let event = client.set_get_eventhandle().map_err(|e| anyhow!("event handle: {e}"))?;
        let capture =
            client.get_audiocaptureclient().map_err(|e| anyhow!("capture client: {e}"))?;
        let block_align = format.get_blockalign() as usize;
        let buffer_frames = client.get_buffer_size().map_err(|e| anyhow!("buffer size: {e}"))?;
        let scratch = vec![0u8; (buffer_frames as usize).max(4096) * block_align];
        Ok(Self { client, event, capture, block_align, scratch })
    }
}

/// Background system-audio capture thread feeding [`AudioPacket`]s to a channel.
pub struct LoopbackCapture {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<Result<()>>,
}

impl LoopbackCapture {
    pub fn start() -> Result<(Self, Receiver<AudioPacket>)> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let stop_flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("trix-audio".into())
            .spawn(move || capture_loop(&stop_flag, &tx))
            .context("failed to spawn audio thread")?;
        Ok((Self { stop, thread }, rx))
    }

    pub fn stop(self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        match self.thread.join() {
            Ok(result) => result,
            Err(_) => bail!("audio capture thread panicked"),
        }
    }
}

fn capture_loop(stop: &AtomicBool, tx: &Sender<AudioPacket>) -> Result<()> {
    wasapi::initialize_mta().ok().context("COM MTA init failed")?;
    // i16 directly: the audio engine autoconverts its float mix, so the
    // packets are already in the encoder's PCM format.
    let format = WaveFormat::new(16, 16, &SampleType::Int, SAMPLE_RATE, CHANNELS, None);
    let mut session = LoopbackSession::open(&format)?;
    session.client.start_stream().map_err(|e| anyhow!("start stream: {e}"))?;

    while !stop.load(Ordering::Relaxed) {
        if session.event.wait_for_event(100).is_err() {
            continue; // system-wide silence: nothing rendering, keep waiting
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
            let packet = AudioPacket {
                qpc_100ns: info.timestamp as i64,
                data: session.scratch[..read_frames as usize * session.block_align].to_vec(),
            };
            if tx.send(packet).is_err() {
                return Ok(()); // consumer gone — recording ended
            }
        }
    }
    session.client.stop_stream().map_err(|e| anyhow!("stop stream: {e}"))?;
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
    let mut session = LoopbackSession::open(&format)?;
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
