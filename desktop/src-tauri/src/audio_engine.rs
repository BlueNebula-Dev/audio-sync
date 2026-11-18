use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, SampleFormat, StreamConfig};
use nnnoiseless::DenoiseState;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sonora::config::EchoCanceller as SonoraEchoCanceller;
use sonora::{AudioProcessing, Config as SonoraConfig, StreamConfig as SonoraStreamConfig};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::protocol::{sequence_diff, FramedPacket};

/// Standard audio operating sample rate (48,000 Hz).
pub const OPUS_SAMPLE_RATE: u32 = 48_000;

/// 5ms stereo frame size at 48kHz (240 samples per channel = 480 interleaved samples = 960 bytes PCM16).
pub const FRAME_SAMPLES_5MS: usize = 240;

/// 10ms frame size at 48kHz (480 samples per channel) — matches RNNoise `DenoiseState::FRAME_SIZE`.
pub const FRAME_SAMPLES_10MS: usize = DenoiseState::FRAME_SIZE; // 480

/// Maximum UDP audio frame bytes (960 bytes for 5ms Stereo 16-bit PCM, well below 1472 MTU).
pub const MAX_AUDIO_FRAME_BYTES: usize = 1400;

#[derive(Debug, Error)]
pub enum AudioEngineError {
    #[error("No default output audio device found for WASAPI loopback capture")]
    NoLoopbackDevice,
    #[error("VB-CABLE Input device not found and fallback output unavailable")]
    NoOutputDevice,
    #[error("Audio device config error: {0}")]
    DeviceConfigError(String),
    #[error("Failed to build audio stream: {0}")]
    BuildStreamError(String),
    #[error("Failed to start audio stream: {0}")]
    PlayStreamError(String),
    #[error("Opus codec error: {0}")]
    OpusError(String),
}

/// Live audio runtime configuration shared between Tauri UI, Global Hotkeys, and DSP threads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioConfig {
    /// Stream PC desktop loopback audio to iPhone.
    pub pc_to_phone_enabled: bool,
    /// Stream iPhone microphone audio to PC VB-CABLE.
    pub phone_to_pc_enabled: bool,
    /// Mute outgoing PC audio.
    pub pc_muted: bool,
    /// Mute incoming iPhone microphone.
    pub mic_muted: bool,
    /// Enable Push-to-Talk mode for the iPhone mic.
    pub push_to_talk_enabled: bool,
    /// Current state of the Push-to-Talk key/button.
    pub push_to_talk_active: bool,
    /// Enable WebRTC AEC3 Acoustic Echo Cancellation using WASAPI loopback as reference.
    pub aec_enabled: bool,
    /// Enable AI noise suppression (RNNoise) on iPhone microphone stream.
    pub rnnoise_enabled: bool,
    /// Voice Activity Detection (VAD) gate threshold (0.0 = disabled, up to 0.95).
    pub vad_gate_threshold: f32,
    /// Microphone gain boost in dB (-12.0 dB to +24.0 dB).
    pub mic_gain_db: f32,
    /// PC loopback volume multiplier (0.0 to 2.0).
    pub pc_volume: f32,
    /// Target Opus bitrate in bits per second (32_000 to 128_000).
    pub opus_bitrate_bps: i32,
    /// Frame duration in milliseconds (10 or 20).
    pub frame_duration_ms: u8,
    /// Jitter buffer target depth in packets (1 to 32).
    pub jitter_buffer_packets: usize,
    /// Preferred virtual mic output device name substring (defaults to "CABLE Input").
    pub virtual_mic_device_hint: String,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            pc_to_phone_enabled: true,
            phone_to_pc_enabled: true,
            pc_muted: false,
            mic_muted: false,
            push_to_talk_enabled: false,
            push_to_talk_active: false,
            aec_enabled: true,
            rnnoise_enabled: true,
            vad_gate_threshold: 0.15,
            mic_gain_db: 0.0,
            pc_volume: 1.0,
            opus_bitrate_bps: 64_000,
            frame_duration_ms: 10,
            jitter_buffer_packets: 8,
            virtual_mic_device_hint: "CABLE Input".to_string(),
        }
    }
}

/// Real-time VU meter & DSP telemetry snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioLevels {
    pub pc_peak: f32,
    pub pc_rms: f32,
    pub mic_peak: f32,
    pub mic_rms: f32,
    pub vad_probability: f32,
}

/// Atomic telemetry storage for lock-free reads from the UI / network loop.
#[derive(Default)]
pub struct AtomicAudioTelemetry {
    pc_peak_bits: AtomicU32,
    pc_rms_bits: AtomicU32,
    mic_peak_bits: AtomicU32,
    mic_rms_bits: AtomicU32,
    vad_prob_bits: AtomicU32,
}

impl AtomicAudioTelemetry {
    pub fn store_pc_levels(&self, peak: f32, rms: f32) {
        self.pc_peak_bits.store(peak.to_bits(), Ordering::Relaxed);
        self.pc_rms_bits.store(rms.to_bits(), Ordering::Relaxed);
    }

    pub fn store_mic_levels(&self, peak: f32, rms: f32, vad: f32) {
        self.mic_peak_bits.store(peak.to_bits(), Ordering::Relaxed);
        self.mic_rms_bits.store(rms.to_bits(), Ordering::Relaxed);
        self.vad_prob_bits.store(vad.to_bits(), Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> AudioLevels {
        AudioLevels {
            pc_peak: f32::from_bits(self.pc_peak_bits.load(Ordering::Relaxed)),
            pc_rms: f32::from_bits(self.pc_rms_bits.load(Ordering::Relaxed)),
            mic_peak: f32::from_bits(self.mic_peak_bits.load(Ordering::Relaxed)),
            mic_rms: f32::from_bits(self.mic_rms_bits.load(Ordering::Relaxed)),
            vad_probability: f32::from_bits(self.vad_prob_bits.load(Ordering::Relaxed)),
        }
    }
}

/// Adaptive UDP Jitter Buffer for incoming Opus microphone packets (`StreamId::PhoneToPc`).
pub struct AdaptiveJitterBuffer {
    packets: BTreeMap<u16, Vec<u8>>,
    next_expected_seq: Option<u16>,
    target_depth: usize,
    max_capacity: usize,
    primed: bool,
}

/// Outcome when popping the next frame from `AdaptiveJitterBuffer`.
pub enum JitterPopResult {
    /// Buffer is still priming or empty prior to stream start.
    NotReady,
    /// Contiguous Opus packet ready for decoding.
    Frame(Vec<u8>),
    /// Sequence gap detected — caller should invoke Opus Packet Loss Concealment (PLC).
    LostPacket,
}

impl AdaptiveJitterBuffer {
    pub fn new(target_depth: usize) -> Self {
        let target_depth = target_depth.clamp(1, 32);
        Self {
            packets: BTreeMap::new(),
            next_expected_seq: None,
            target_depth,
            max_capacity: target_depth * 6,
            primed: false,
        }
    }

    pub fn set_target_depth(&mut self, depth: usize) {
        self.target_depth = depth.clamp(1, 32);
        self.max_capacity = self.target_depth * 6;
    }

    pub fn push(&mut self, seq: u16, opus_payload: Vec<u8>) {
        if let Some(expected) = self.next_expected_seq {
            let diff = sequence_diff(seq, expected);
            if diff < -32 {
                // Stream likely restarted on the iPhone client; reset sequence tracking.
                self.packets.clear();
                self.next_expected_seq = Some(seq);
                self.primed = false;
            } else if diff < 0 {
                // Late packet that has already been concealed/played; drop it.
                return;
            }
        } else {
            self.next_expected_seq = Some(seq);
        }

        self.packets.insert(seq, opus_payload);

        // Bound maximum jitter buffer memory if a burst arrives.
        while self.packets.len() > self.max_capacity {
            if let Some(expected) = self.next_expected_seq {
                self.packets.remove(&expected);
                self.next_expected_seq = Some(expected.wrapping_add(1));
            } else if let Some((&first_key, _)) = self.packets.iter().next() {
                self.packets.remove(&first_key);
            }
        }

        if self.packets.len() >= self.target_depth {
            self.primed = true;
        }
    }

    pub fn reset(&mut self) {
        self.packets.clear();
        self.next_expected_seq = None;
        self.primed = false;
    }

    pub fn pop_next(&mut self) -> JitterPopResult {
        if !self.primed {
            return JitterPopResult::NotReady;
        }

        let Some(expected) = self.next_expected_seq else {
            self.primed = false;
            return JitterPopResult::NotReady;
        };

        if let Some(payload) = self.packets.remove(&expected) {
            self.next_expected_seq = Some(expected.wrapping_add(1));
            // Note: Do NOT un-prime if queue is empty; transient empty queue during normal packet flow
            // should not stall playback for an entire target_depth refill cycle!
            return JitterPopResult::Frame(payload);
        }

        // If the expected packet is missing, check if newer packets have already arrived.
        if !self.packets.is_empty() {
            self.next_expected_seq = Some(expected.wrapping_add(1));
            return JitterPopResult::LostPacket;
        }

        // Buffer is temporarily starved of future packets; un-prime so it buffers incoming bursts
        self.primed = false;
        JitterPopResult::NotReady
    }
}

/// Shared 48kHz mono PCM ring buffer & Adaptive Sample Rate Converter (ASRC) state.
pub struct OutputRingState {
    pub ring: VecDeque<f32>,
    pub primed: bool,
    pub frac_pos: f64,
}

impl OutputRingState {
    pub fn new() -> Self {
        Self {
            ring: VecDeque::with_capacity(OPUS_SAMPLE_RATE as usize),
            primed: false,
            frac_pos: 0.0,
        }
    }
}

impl Default for OutputRingState {
    fn default() -> Self {
        Self::new()
    }
}

/// Stateful DSP processor combining WebRTC AEC3, Opus decoding, RNNoise AI denoising, VAD gate, and Gain boost.
pub struct MicDspPipeline {
    rnnoise: Box<DenoiseState<'static>>,
    aec: Option<AudioProcessing>,
    jitter: AdaptiveJitterBuffer,
    rnnoise_input_fifo: VecDeque<f32>,
    render_staging_fifo: VecDeque<[f32; FRAME_SAMPLES_10MS]>,
    vad_envelope: f32,
}

impl MicDspPipeline {
    pub fn new(jitter_depth: usize) -> Result<Self, AudioEngineError> {
        let aec = {
            let config = SonoraConfig {
                echo_canceller: Some(SonoraEchoCanceller::default()),
                ..Default::default()
            };
            Some(
                AudioProcessing::builder()
                    .config(config)
                    .capture_config(SonoraStreamConfig::new(OPUS_SAMPLE_RATE, 1))
                    .render_config(SonoraStreamConfig::new(OPUS_SAMPLE_RATE, 1))
                    .build(),
            )
        };

        Ok(Self {
            rnnoise: DenoiseState::new(),
            aec,
            jitter: AdaptiveJitterBuffer::new(jitter_depth),
            rnnoise_input_fifo: VecDeque::with_capacity(FRAME_SAMPLES_10MS * 4),
            render_staging_fifo: VecDeque::with_capacity(48),
            vad_envelope: 1.0,
        })
    }

    pub fn reset(&mut self) {
        self.jitter.reset();
        self.rnnoise_input_fifo.clear();
        self.render_staging_fifo.clear();
        self.vad_envelope = 1.0;
    }

    /// Pushes an incoming UDP `PhoneToPc` packet into the jitter buffer.
    pub fn ingest_packet(&mut self, packet: FramedPacket, target_jitter_depth: usize) {
        self.jitter.set_target_depth(target_jitter_depth);
        self.jitter
            .push(packet.header.sequence_num, packet.payload);
    }

    /// Ingests available jitter-buffered packets (16-bit PCM Mono), runs WebRTC AEC3, RNNoise + Gain Boost,
    /// and pushes 48kHz mono samples into `output_ring`.
    pub fn process_available(
        &mut self,
        config: &AudioConfig,
        telemetry: &AtomicAudioTelemetry,
        output_ring: &Mutex<OutputRingState>,
        render_ref_rx: &std::sync::mpsc::Receiver<[f32; FRAME_SAMPLES_10MS]>,
    ) {
        // Stage pending 10ms render reference frames from WASAPI loopback (bounded to ~400ms)
        if config.aec_enabled {
            while let Ok(render_frame) = render_ref_rx.try_recv() {
                self.render_staging_fifo.push_back(render_frame);
            }
            while self.render_staging_fifo.len() > 40 {
                self.render_staging_fifo.pop_front();
            }
        } else {
            while render_ref_rx.try_recv().is_ok() {}
            self.render_staging_fifo.clear();
        }

        loop {
            match self.jitter.pop_next() {
                JitterPopResult::NotReady => break,
                JitterPopResult::Frame(pcm16_bytes) => {
                    for chunk in pcm16_bytes.chunks_exact(2) {
                        let s16 = i16::from_le_bytes([chunk[0], chunk[1]]);
                        self.rnnoise_input_fifo.push_back(s16 as f32 / 32768.0);
                    }
                }
                JitterPopResult::LostPacket => {
                    // Smooth concealment: smoothly decay from the last valid sample rather than abrupt silence step
                    let last_sample = self.rnnoise_input_fifo.back().copied().unwrap_or(0.0);
                    for i in 0..FRAME_SAMPLES_10MS {
                        let decay = 1.0 - (i as f32 / FRAME_SAMPLES_10MS as f32);
                        self.rnnoise_input_fifo.push_back(last_sample * decay * 0.7);
                    }
                }
            }
        }

        // Check mute / Push-to-Talk state
        let is_active = config.phone_to_pc_enabled
            && !config.mic_muted
            && (!config.push_to_talk_enabled || config.push_to_talk_active);

        let gain_linear = 10.0_f32.powf(config.mic_gain_db / 20.0);

        // Process in exact 480-sample (10ms) frames required by AEC3 & RNNoise
        while self.rnnoise_input_fifo.len() >= FRAME_SAMPLES_10MS {
            let mut raw_frame = [0.0f32; FRAME_SAMPLES_10MS];
            for slot in &mut raw_frame {
                *slot = self.rnnoise_input_fifo.pop_front().unwrap_or(0.0);
            }

            if !is_active {
                if config.aec_enabled && !self.render_staging_fifo.is_empty() {
                    let _ = self.render_staging_fifo.pop_front();
                }
                telemetry.store_mic_levels(0.0, 0.0, 0.0);
                let mut guard = output_ring.lock();
                for _ in 0..FRAME_SAMPLES_10MS {
                    if guard.ring.len() < OPUS_SAMPLE_RATE as usize {
                        guard.ring.push_back(0.0);
                    }
                }
                continue;
            }

            // 1. WebRTC AEC3 Acoustic Echo Cancellation (synchronized 1:1 10ms render/capture cadence)
            let mut aec_cleaned = [0.0f32; FRAME_SAMPLES_10MS];
            if config.aec_enabled {
                if let Some(apm) = &mut self.aec {
                    // Gently trim excess stale backlog if render queue drifted ahead
                    if self.render_staging_fifo.len() > 20 {
                        self.render_staging_fifo.pop_front();
                    }
                    let render_frame = self
                        .render_staging_fifo
                        .pop_front()
                        .unwrap_or([0.0f32; FRAME_SAMPLES_10MS]);
                    let mut render_out = [0.0f32; FRAME_SAMPLES_10MS];
                    let _ = apm.process_render_f32(&[&render_frame], &mut [&mut render_out]);

                    let delay_ms = ((config.jitter_buffer_packets * 10) + 30).clamp(0, 500) as i32;
                    let _ = apm.set_stream_delay_ms(delay_ms);

                    let mut dest = [0.0f32; FRAME_SAMPLES_10MS];
                    if apm.process_capture_f32(&[&raw_frame], &mut [&mut dest]).is_ok() {
                        aec_cleaned.copy_from_slice(&dest);
                    } else {
                        aec_cleaned.copy_from_slice(&raw_frame);
                    }
                } else {
                    aec_cleaned.copy_from_slice(&raw_frame);
                }
            } else {
                aec_cleaned.copy_from_slice(&raw_frame);
            }

            // 2. AI Noise Suppression (RNNoise) on AEC-cleaned stream with smoothed VAD gate
            let mut processed_frame = [0.0f32; FRAME_SAMPLES_10MS];
            let vad_prob = if config.rnnoise_enabled {
                // `nnnoiseless` operates on 16-bit PCM amplitude scale (-32768.0..32767.0)
                let mut scaled_in = [0.0f32; FRAME_SAMPLES_10MS];
                let mut scaled_out = [0.0f32; FRAME_SAMPLES_10MS];
                for i in 0..FRAME_SAMPLES_10MS {
                    scaled_in[i] = (aec_cleaned[i] * 32768.0).clamp(-32768.0, 32767.0);
                }
                let vad = self.rnnoise.process_frame(&mut scaled_out, &scaled_in);
                let gate_factor = if config.vad_gate_threshold <= 0.001 {
                    self.vad_envelope = 1.0;
                    1.0
                } else {
                    let target_gate = if vad >= config.vad_gate_threshold {
                        1.0
                    } else {
                        let ratio = (vad / config.vad_gate_threshold.max(0.01)).clamp(0.0, 1.0);
                        (ratio * ratio).max(0.05)
                    };
                    if target_gate >= self.vad_envelope {
                        self.vad_envelope = 0.35 * self.vad_envelope + 0.65 * target_gate;
                    } else {
                        self.vad_envelope = 0.82 * self.vad_envelope + 0.18 * target_gate;
                    }
                    self.vad_envelope.max(0.05)
                };
                for i in 0..FRAME_SAMPLES_10MS {
                    processed_frame[i] = (scaled_out[i] / 32768.0) * gate_factor;
                }
                vad
            } else {
                self.vad_envelope = 1.0;
                processed_frame.copy_from_slice(&aec_cleaned);
                1.0
            };

            // Apply Gain Boost with smooth tanh soft-clipping and compute VU metrics
            let mut peak = 0.0f32;
            let mut sum_sq = 0.0f32;
            for sample in &mut processed_frame {
                let boosted = (*sample * gain_linear).tanh();
                let abs = boosted.abs();
                if abs > peak {
                    peak = abs;
                }
                sum_sq += boosted * boosted;
                *sample = boosted;
            }
            let rms = (sum_sq / FRAME_SAMPLES_10MS as f32).sqrt();
            telemetry.store_mic_levels(peak, rms, vad_prob);

            let mut guard = output_ring.lock();
            // Bound ring buffer latency to 500ms max (24,000 samples at 48kHz) so bursts of 100-150ms are never trimmed
            let max_ring_samples = (OPUS_SAMPLE_RATE as usize * 50) / 100;
            while guard.ring.len() + FRAME_SAMPLES_10MS > max_ring_samples {
                guard.ring.pop_front();
            }
            guard.ring.extend(processed_frame);
        }
    }
}

/// Calculates Peak and RMS values for an interleaved stereo or mono slice.
#[inline]
pub fn compute_peak_and_rms(samples: &[f32]) -> (f32, f32) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let mut peak = 0.0f32;
    let mut sum_sq = 0.0f32;
    for &s in samples {
        let a = s.abs();
        if a > peak {
            peak = a;
        }
        sum_sq += s * s;
    }
    let rms = (sum_sq / samples.len() as f32).sqrt();
    (peak, rms)
}

/// Finds the VB-CABLE Virtual Audio Device (`"CABLE Input"`) or falls back to default output.
pub fn find_vbcable_output_device(hint: &str) -> Result<(Device, String, bool), AudioEngineError> {
    let host = cpal::default_host();
    let search_hint = if hint.trim().is_empty() {
        "CABLE Input"
    } else {
        hint.trim()
    };
    let search_lower = search_hint.to_lowercase();

    if let Ok(devices) = host.output_devices() {
        for dev in devices {
            if let Ok(name) = dev.name() {
                let name_lower = name.to_lowercase();
                if name_lower.contains(&search_lower)
                    || name_lower.contains("vb-audio")
                    || name_lower.contains("cable input")
                {
                    return Ok((dev, name, true));
                }
            }
        }
    }

    // Fallback to default output device if VB-CABLE is not installed yet
    let fallback = host
        .default_output_device()
        .ok_or(AudioEngineError::NoOutputDevice)?;
    let name = fallback
        .name()
        .unwrap_or_else(|_| "Default Output Device".to_string());
    Ok((fallback, name, false))
}

/// Lists all available output audio devices on Windows (for UI selector & diagnostics).
pub fn list_output_devices() -> Vec<String> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    if let Ok(devices) = host.output_devices() {
        for dev in devices {
            if let Ok(name) = dev.name() {
                names.push(name);
            }
        }
    }
    names
}

/// High-level AudioEngine controller managing WASAPI loopback capture, Mic DSP, and VB-CABLE output.
pub struct AudioEngine {
    pub config: Arc<Mutex<AudioConfig>>,
    pub telemetry: Arc<AtomicAudioTelemetry>,
    running: Arc<AtomicBool>,
    vbcable_device_name: Arc<Mutex<String>>,
    vbcable_detected: Arc<AtomicBool>,
}

impl AudioEngine {
    pub fn new(config: Arc<Mutex<AudioConfig>>) -> Self {
        Self {
            config,
            telemetry: Arc::new(AtomicAudioTelemetry::default()),
            running: Arc::new(AtomicBool::new(false)),
            vbcable_device_name: Arc::new(Mutex::new(String::from("Not initialized"))),
            vbcable_detected: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn vbcable_status(&self) -> (String, bool) {
        (
            self.vbcable_device_name.lock().clone(),
            self.vbcable_detected.load(Ordering::Relaxed),
        )
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Starts both the WASAPI desktop loopback capture thread (`PcToPhone`) and the
    /// incoming microphone DSP + VB-CABLE output thread (`PhoneToPc`).
    pub fn start(
        &self,
        outbound_opus_tx: mpsc::Sender<Vec<u8>>,
        mut inbound_mic_rx: mpsc::Receiver<FramedPacket>,
    ) -> Result<(), AudioEngineError> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let (render_ref_tx, render_ref_rx) =
            std::sync::mpsc::sync_channel::<[f32; FRAME_SAMPLES_10MS]>(64);

        let running_loopback = Arc::clone(&self.running);
        let config_loopback = Arc::clone(&self.config);
        let telemetry_loopback = Arc::clone(&self.telemetry);

        // 1. Spawn WASAPI Loopback Capture + Stereo Opus Encoder thread (PC -> iPhone)
        std::thread::Builder::new()
            .name("wasapi-loopback-opus".to_string())
            .spawn(move || {
                if let Err(err) = run_wasapi_loopback_worker(
                    running_loopback,
                    config_loopback,
                    telemetry_loopback,
                    outbound_opus_tx,
                    render_ref_tx,
                ) {
                    log::error!("WASAPI loopback worker exited with error: {err}");
                }
            })
            .map_err(|e| AudioEngineError::BuildStreamError(e.to_string()))?;

        // 2. Spawn Incoming Mic DSP (Opus Decode + RNNoise + Gain) & VB-CABLE Output thread
        let running_mic = Arc::clone(&self.running);
        let config_mic = Arc::clone(&self.config);
        let telemetry_mic = Arc::clone(&self.telemetry);
        let vbcable_name_slot = Arc::clone(&self.vbcable_device_name);
        let vbcable_flag_slot = Arc::clone(&self.vbcable_detected);

        std::thread::Builder::new()
            .name("vbcable-mic-dsp".to_string())
            .spawn(move || {
                if let Err(err) = run_vbcable_mic_worker(
                    running_mic,
                    config_mic,
                    telemetry_mic,
                    vbcable_name_slot,
                    vbcable_flag_slot,
                    &mut inbound_mic_rx,
                    render_ref_rx,
                ) {
                    log::error!("VB-CABLE mic worker exited with error: {err}");
                }
            })
            .map_err(|e| AudioEngineError::BuildStreamError(e.to_string()))?;

        Ok(())
    }
}

/// Dedicated worker thread that opens Windows WASAPI loopback on the default output device,
/// resamples/downmixes to 48kHz stereo, encodes low-delay Opus frames, and sends them to the UDP task.
fn run_wasapi_loopback_worker(
    running: Arc<AtomicBool>,
    config: Arc<Mutex<AudioConfig>>,
    telemetry: Arc<AtomicAudioTelemetry>,
    outbound_opus_tx: mpsc::Sender<Vec<u8>>,
    render_ref_tx: std::sync::mpsc::SyncSender<[f32; FRAME_SAMPLES_10MS]>,
) -> Result<(), AudioEngineError> {
    let host = cpal::default_host();
    // On Windows WASAPI, building an *input* stream on an *output* device opens it in Loopback mode!
    let loopback_device = host
        .default_output_device()
        .ok_or(AudioEngineError::NoLoopbackDevice)?;

    let supported_cfg = loopback_device
        .default_output_config()
        .map_err(|e| AudioEngineError::DeviceConfigError(e.to_string()))?;

    let sample_format = supported_cfg.sample_format();
    let device_rate = supported_cfg.sample_rate().0;
    let device_channels = supported_cfg.channels() as usize;

    // Request a low-latency 5ms buffer (e.g. 240 frames at 48kHz) with automatic fallback
    let preferred_frames = (device_rate / 200).max(128);
    let low_latency_cfg = StreamConfig {
        channels: supported_cfg.channels(),
        sample_rate: supported_cfg.sample_rate(),
        buffer_size: BufferSize::Fixed(preferred_frames),
    };
    let default_cfg: StreamConfig = supported_cfg.into();

    let (pcm_tx, pcm_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(64);

    let err_fn = |err| {
        log::warn!("WASAPI loopback stream error: {err}");
    };

    let build_loopback = |stream_cfg: &StreamConfig| -> Result<cpal::Stream, cpal::BuildStreamError> {
        match sample_format {
            SampleFormat::F32 => {
                let tx = pcm_tx.clone();
                loopback_device.build_input_stream(
                    stream_cfg,
                    move |data: &[f32], _| {
                        let _ = tx.try_send(data.to_vec());
                    },
                    err_fn,
                    None,
                )
            }
            SampleFormat::I16 => {
                let tx = pcm_tx.clone();
                loopback_device.build_input_stream(
                    stream_cfg,
                    move |data: &[i16], _| {
                        let converted: Vec<f32> =
                            data.iter().map(|&s| s as f32 / 32768.0).collect();
                        let _ = tx.try_send(converted);
                    },
                    err_fn,
                    None,
                )
            }
            SampleFormat::U16 => {
                let tx = pcm_tx.clone();
                loopback_device.build_input_stream(
                    stream_cfg,
                    move |data: &[u16], _| {
                        let converted: Vec<f32> = data
                            .iter()
                            .map(|&s| (s as f32 - 32768.0) / 32768.0)
                            .collect();
                        let _ = tx.try_send(converted);
                    },
                    err_fn,
                    None,
                )
            }
            _ => Err(cpal::BuildStreamError::StreamConfigNotSupported),
        }
    };

    let stream = build_loopback(&low_latency_cfg)
        .or_else(|_| build_loopback(&default_cfg))
        .map_err(|e| AudioEngineError::BuildStreamError(e.to_string()))?;

    stream
        .play()
        .map_err(|e| AudioEngineError::PlayStreamError(e.to_string()))?;

    let mut stereo_48k_fifo: VecDeque<f32> = VecDeque::with_capacity(OPUS_SAMPLE_RATE as usize * 2);
    let mut mono_render_fifo: VecDeque<f32> = VecDeque::with_capacity(FRAME_SAMPLES_10MS * 4);
    let mut stream_paused = false;

    while running.load(Ordering::Relaxed) {
        let cfg_snapshot = config.lock().clone();
        let need_aec = cfg_snapshot.aec_enabled && cfg_snapshot.phone_to_pc_enabled;
        let need_loopback = cfg_snapshot.pc_to_phone_enabled || need_aec;

        if !need_loopback {
            if !stream_paused {
                let _ = stream.pause();
                stream_paused = true;
            }
            telemetry.store_pc_levels(0.0, 0.0);
            stereo_48k_fifo.clear();
            mono_render_fifo.clear();
            while pcm_rx.try_recv().is_ok() {}
            std::thread::sleep(std::time::Duration::from_millis(25));
            continue;
        } else if stream_paused {
            let _ = stream.play();
            stream_paused = false;
        }

        match pcm_rx.recv_timeout(std::time::Duration::from_millis(25)) {
            Ok(raw_interleaved) => {
                let cfg_snapshot = config.lock().clone();
                let need_opus = cfg_snapshot.pc_to_phone_enabled && !cfg_snapshot.pc_muted;
                let need_aec = cfg_snapshot.aec_enabled && cfg_snapshot.phone_to_pc_enabled;

                if !need_opus && !need_aec {
                    telemetry.store_pc_levels(0.0, 0.0);
                    stereo_48k_fifo.clear();
                    mono_render_fifo.clear();
                    continue;
                }

                // Convert arbitrary channel count & sample rate to 48kHz Stereo interleaved
                convert_to_48k_stereo(
                    &raw_interleaved,
                    device_channels,
                    device_rate,
                    cfg_snapshot.pc_volume,
                    &mut stereo_48k_fifo,
                );

                // 5ms = 240 stereo frames (480 interleaved f32 = 960 bytes PCM16)
                let frame_samples_per_ch = FRAME_SAMPLES_5MS;
                let interleaved_frame_len = frame_samples_per_ch * 2;
                let mut pcm_frame = [0.0f32; FRAME_SAMPLES_5MS * 2];
                let mut render_10ms = [0.0f32; FRAME_SAMPLES_10MS];

                while stereo_48k_fifo.len() >= interleaved_frame_len {
                    for slot in &mut pcm_frame {
                        *slot = stereo_48k_fifo.pop_front().unwrap_or(0.0);
                    }

                    let (peak, rms) = compute_peak_and_rms(&pcm_frame);
                    if need_opus {
                        telemetry.store_pc_levels(peak, rms);
                    } else {
                        telemetry.store_pc_levels(0.0, 0.0);
                    }

                    // Feed 10ms mono frames to AEC3 render reference channel
                    if need_aec {
                        for i in 0..frame_samples_per_ch {
                            let l = pcm_frame[i * 2];
                            let r = pcm_frame[i * 2 + 1];
                            mono_render_fifo.push_back((l + r) * 0.5);
                        }
                        while mono_render_fifo.len() >= FRAME_SAMPLES_10MS {
                            for slot in &mut render_10ms {
                                *slot = mono_render_fifo.pop_front().unwrap_or(0.0);
                            }
                            let _ = render_ref_tx.try_send(render_10ms);
                        }
                    }

                    // Pack into pristine 16-bit Little-Endian PCM (480 samples * 2 = 960 bytes, zero codec mismatch)
                    if need_opus {
                        let mut pcm16_bytes = Vec::with_capacity(interleaved_frame_len * 2);
                        for &s in &pcm_frame {
                            let clamped = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                            pcm16_bytes.extend_from_slice(&clamped.to_le_bytes());
                        }
                        let _ = outbound_opus_tx.try_send(pcm16_bytes);
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                telemetry.store_pc_levels(0.0, 0.0);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    Ok(())
}

/// Dedicated worker thread that receives incoming iPhone mic Opus packets, runs RNNoise + Gain DSP,
/// and streams the audio into the VB-CABLE Virtual Audio Device.
fn run_vbcable_mic_worker(
    running: Arc<AtomicBool>,
    config: Arc<Mutex<AudioConfig>>,
    telemetry: Arc<AtomicAudioTelemetry>,
    vbcable_name_slot: Arc<Mutex<String>>,
    vbcable_flag_slot: Arc<AtomicBool>,
    inbound_mic_rx: &mut mpsc::Receiver<FramedPacket>,
    render_ref_rx: std::sync::mpsc::Receiver<[f32; FRAME_SAMPLES_10MS]>,
) -> Result<(), AudioEngineError> {
    let hint = config.lock().virtual_mic_device_hint.clone();
    let (output_device, dev_name, is_vbcable) = find_vbcable_output_device(&hint)?;
    *vbcable_name_slot.lock() = dev_name;
    vbcable_flag_slot.store(is_vbcable, Ordering::Relaxed);

    let supported_cfg = output_device
        .default_output_config()
        .map_err(|e| AudioEngineError::DeviceConfigError(e.to_string()))?;

    let out_format = supported_cfg.sample_format();
    let out_rate = supported_cfg.sample_rate().0;
    let out_channels = supported_cfg.channels() as usize;

    let preferred_frames = (out_rate / 200).max(128);
    let low_latency_cfg = StreamConfig {
        channels: supported_cfg.channels(),
        sample_rate: supported_cfg.sample_rate(),
        buffer_size: BufferSize::Fixed(preferred_frames),
    };
    let default_cfg: StreamConfig = supported_cfg.into();

    // Shared 48kHz mono PCM ring buffer between DSP processor and VB-CABLE output callback
    let output_ring: Arc<Mutex<OutputRingState>> =
        Arc::new(Mutex::new(OutputRingState::new()));

    let err_fn = |err| {
        log::warn!("VB-CABLE output stream error: {err}");
    };

    let build_output = |stream_cfg: &StreamConfig| -> Result<cpal::Stream, cpal::BuildStreamError> {
        let ring = Arc::clone(&output_ring);
        match out_format {
            SampleFormat::F32 => output_device.build_output_stream(
                stream_cfg,
                move |data: &mut [f32], _| {
                    if !is_vbcable {
                        // Safety silence: never blast mic into main PC speakers to prevent feedback loop
                        data.fill(0.0);
                    } else {
                        fill_output_buffer_f32(data, out_channels, out_rate, &ring);
                    }
                },
                err_fn,
                None,
            ),
            SampleFormat::I16 => {
                let mut temp = Vec::<f32>::with_capacity(4096);
                output_device.build_output_stream(
                    stream_cfg,
                    move |data: &mut [i16], _| {
                        if !is_vbcable {
                            data.fill(0);
                        } else {
                            temp.resize(data.len(), 0.0);
                            fill_output_buffer_f32(&mut temp, out_channels, out_rate, &ring);
                            for (dst, &src) in data.iter_mut().zip(temp.iter()) {
                                *dst = (src * 32767.0).clamp(-32768.0, 32767.0) as i16;
                            }
                        }
                    },
                    err_fn,
                    None,
                )
            }
            SampleFormat::U16 => {
                let mut temp = Vec::<f32>::with_capacity(4096);
                output_device.build_output_stream(
                    stream_cfg,
                    move |data: &mut [u16], _| {
                        if !is_vbcable {
                            data.fill(32768);
                        } else {
                            temp.resize(data.len(), 0.0);
                            fill_output_buffer_f32(&mut temp, out_channels, out_rate, &ring);
                            for (dst, &src) in data.iter_mut().zip(temp.iter()) {
                                *dst = ((src * 32767.0) + 32768.0).clamp(0.0, 65535.0) as u16;
                            }
                        }
                    },
                    err_fn,
                    None,
                )
            }
            _ => Err(cpal::BuildStreamError::StreamConfigNotSupported),
        }
    };

    let stream = match build_output(&low_latency_cfg) {
        Ok(s) => s,
        Err(e) => {
            log::info!("Low-latency VB-CABLE config rejected ({e}); falling back to default config");
            build_output(&default_cfg)
                .map_err(|e| AudioEngineError::BuildStreamError(e.to_string()))?
        }
    };

    stream
        .play()
        .map_err(|e| AudioEngineError::PlayStreamError(e.to_string()))?;

    let initial_jitter = config.lock().jitter_buffer_packets.clamp(1, 32);
    let mut dsp = MicDspPipeline::new(initial_jitter)?;
    let mut stream_paused = false;

    while running.load(Ordering::Relaxed) {
        let cfg_snapshot = config.lock().clone();
        if !cfg_snapshot.phone_to_pc_enabled {
            if !stream_paused {
                let _ = stream.pause();
                stream_paused = true;
            }
            telemetry.store_mic_levels(0.0, 0.0, 0.0);
            dsp.reset();
            {
                let mut guard = output_ring.lock();
                guard.ring.clear();
                guard.primed = false;
                guard.frac_pos = 0.0;
            }
            while inbound_mic_rx.try_recv().is_ok() {}
            while render_ref_rx.try_recv().is_ok() {}
            std::thread::sleep(std::time::Duration::from_millis(25));
            continue;
        } else if stream_paused {
            let _ = stream.play();
            stream_paused = false;
        }

        match inbound_mic_rx.try_recv() {
            Ok(pkt) => {
                let jitter_depth = cfg_snapshot.jitter_buffer_packets.clamp(1, 32);
                dsp.ingest_packet(pkt, jitter_depth);
                // Drain any immediately queued packets before running DSP
                while let Ok(more_pkt) = inbound_mic_rx.try_recv() {
                    dsp.ingest_packet(more_pkt, jitter_depth);
                }
                dsp.process_available(&cfg_snapshot, &telemetry, &output_ring, &render_ref_rx);
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(std::time::Duration::from_millis(3));
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
        }
    }

    Ok(())
}

/// Converts interleaved multi-channel input at `src_rate` into 48kHz stereo interleaved samples.
fn convert_to_48k_stereo(
    input: &[f32],
    channels: usize,
    src_rate: u32,
    volume: f32,
    out_fifo: &mut VecDeque<f32>,
) {
    if channels == 0 || input.is_empty() {
        return;
    }
    let in_frames = input.len() / channels;
    if in_frames == 0 {
        return;
    }

    if src_rate == OPUS_SAMPLE_RATE {
        for frame_idx in 0..in_frames {
            let base = frame_idx * channels;
            let l = (input[base] * volume).clamp(-1.0, 1.0);
            let r = if channels >= 2 {
                (input[base + 1] * volume).clamp(-1.0, 1.0)
            } else {
                l
            };
            out_fifo.push_back(l);
            out_fifo.push_back(r);
        }
    } else {
        // Linear resampling to 48kHz
        let ratio = src_rate as f64 / OPUS_SAMPLE_RATE as f64;
        let out_frames = ((in_frames as f64) / ratio).floor() as usize;
        for out_idx in 0..out_frames {
            let src_pos = out_idx as f64 * ratio;
            let idx0 = (src_pos.floor() as usize).min(in_frames - 1);
            let idx1 = (idx0 + 1).min(in_frames - 1);
            let frac = (src_pos - idx0 as f64) as f32;

            let base0 = idx0 * channels;
            let base1 = idx1 * channels;

            let l0 = input[base0];
            let l1 = input[base1];
            let l = ((l0 + (l1 - l0) * frac) * volume).clamp(-1.0, 1.0);

            let r = if channels >= 2 {
                let r0 = input[base0 + 1];
                let r1 = input[base1 + 1];
                ((r0 + (r1 - r0) * frac) * volume).clamp(-1.0, 1.0)
            } else {
                l
            };

            out_fifo.push_back(l);
            out_fifo.push_back(r);
        }
    }
}

const PRIME_THRESHOLD_SAMPLES: usize = 7200; // 150ms pre-roll cushion to absorb 100ms iPhone capture bursts

/// Fills the VB-CABLE output slice from the 48kHz mono ring buffer at exact 1.0x pitch (no pitch warping),
/// duplicating across output channels and cleanly handling sample rate conversion if the output device is not 48kHz.
fn fill_output_buffer_f32(
    out: &mut [f32],
    out_channels: usize,
    out_rate: u32,
    ring_state: &Mutex<OutputRingState>,
) {
    if out_channels == 0 || out.is_empty() {
        return;
    }
    let out_frames = out.len() / out_channels;
    let mut guard = ring_state.lock();

    // 1. Initial pre-roll / recovery priming (ensures at least 150ms buffer before output starts)
    if !guard.primed {
        if guard.ring.len() >= PRIME_THRESHOLD_SAMPLES {
            guard.primed = true;
        } else {
            // Still priming: output silence
            out.fill(0.0);
            return;
        }
    }

    // 2. Playback: 1:1 direct copy at 48kHz (ZERO pitch warping or vibrato), or clean resample if device is 44.1kHz
    if out_rate == OPUS_SAMPLE_RATE {
        for frame_idx in 0..out_frames {
            if let Some(sample) = guard.ring.pop_front() {
                let base = frame_idx * out_channels;
                for ch in 0..out_channels {
                    out[base + ch] = sample;
                }
            } else {
                // Buffer ran empty: un-prime so we rebuild pre-roll cushion cleanly
                guard.primed = false;
                for i in (frame_idx * out_channels)..out.len() {
                    out[i] = 0.0;
                }
                break;
            }
        }
    } else {
        // Linear interpolation without pitch modulation (constant ratio)
        let step = OPUS_SAMPLE_RATE as f64 / out_rate as f64;
        for frame_idx in 0..out_frames {
            if guard.ring.is_empty() {
                guard.primed = false;
                guard.frac_pos = 0.0;
                for i in (frame_idx * out_channels)..out.len() {
                    out[i] = 0.0;
                }
                break;
            }

            let s0 = guard.ring.front().copied().unwrap_or(0.0);
            let s1 = if guard.ring.len() > 1 {
                guard.ring.get(1).copied().unwrap_or(s0)
            } else {
                s0
            };

            let interp_sample = s0 + (s1 - s0) * (guard.frac_pos as f32);
            let base = frame_idx * out_channels;
            for ch in 0..out_channels {
                out[base + ch] = interp_sample;
            }

            guard.frac_pos += step;
            while guard.frac_pos >= 1.0 {
                guard.ring.pop_front();
                guard.frac_pos -= 1.0;
                if guard.ring.is_empty() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{PacketType, StreamId};

    #[test]
    fn test_pcm16_ingest_and_rnnoise_pipeline() {
        // Generate 10ms 440Hz sine wave at 48kHz (480 samples)
        let mut pcm16_bytes = Vec::with_capacity(FRAME_SAMPLES_10MS * 2);
        for i in 0..FRAME_SAMPLES_10MS {
            let t = i as f32 / OPUS_SAMPLE_RATE as f32;
            let sample = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5;
            let s16 = (sample * 32767.0) as i16;
            pcm16_bytes.extend_from_slice(&s16.to_le_bytes());
        }

        let mut dsp = MicDspPipeline::new(1).expect("DSP pipeline init");
        let pkt = FramedPacket::new(
            PacketType::AudioOpus,
            StreamId::PhoneToPc,
            1,
            100,
            pcm16_bytes,
        );

        let cfg = AudioConfig::default();
        let telemetry = AtomicAudioTelemetry::default();
        let output_ring = Mutex::new(OutputRingState::new());
        let (_render_tx, render_rx) = std::sync::mpsc::sync_channel(1);

        dsp.ingest_packet(pkt, 1);
        dsp.process_available(&cfg, &telemetry, &output_ring, &render_rx);

        assert_eq!(output_ring.lock().ring.len(), FRAME_SAMPLES_10MS);
    }

    #[test]
    fn test_aec3_echo_suppression_pipeline() {
        const TOTAL_FRAMES: usize = 80;
        const CONVERGE_START: usize = 50;
        const ECHO_DELAY_FRAMES: usize = 3;

        let mut dsp = MicDspPipeline::new(1).expect("DSP pipeline init");
        let (render_tx, render_rx) = std::sync::mpsc::sync_channel(16);

        let cfg = AudioConfig {
            aec_enabled: true,
            rnnoise_enabled: false,
            vad_gate_threshold: 0.0,
            jitter_buffer_packets: 1,
            ..Default::default()
        };
        let telemetry = AtomicAudioTelemetry::default();
        let output_ring = Mutex::new(OutputRingState::new());

        let mut rng_state: u64 = 0x1234_5678_9ABC_DEF0;
        let mut delay_line: VecDeque<[f32; FRAME_SAMPLES_10MS]> =
            VecDeque::from(vec![[0.0f32; FRAME_SAMPLES_10MS]; ECHO_DELAY_FRAMES]);

        let mut input_energy_sq: f64 = 0.0;
        let mut output_energy_sq: f64 = 0.0;
        let mut measured_samples: usize = 0;

        for frame_idx in 0..TOTAL_FRAMES {
            // Deterministic broadband render excitation
            let mut render_frame = [0.0f32; FRAME_SAMPLES_10MS];
            for sample in &mut render_frame {
                rng_state ^= rng_state << 13;
                rng_state ^= rng_state >> 7;
                rng_state ^= rng_state << 17;
                let norm = ((rng_state & 0x7FFF_FFFF) as f32 / 0x4000_0000u32 as f32) - 1.0;
                *sample = norm * 0.4;
            }
            delay_line.push_back(render_frame);
            let delayed_render = delay_line.pop_front().unwrap();
            render_tx.send(render_frame).unwrap();

            // Capture receives attenuated delayed speaker echo (0.5x)
            let mut echo_pcm16 = Vec::with_capacity(FRAME_SAMPLES_10MS * 2);
            for &s in &delayed_render {
                let echo_s = s * 0.5;
                if frame_idx >= CONVERGE_START {
                    input_energy_sq += (echo_s as f64) * (echo_s as f64);
                }
                let s16 = (echo_s.clamp(-1.0, 1.0) * 32767.0) as i16;
                echo_pcm16.extend_from_slice(&s16.to_le_bytes());
            }

            let pkt = FramedPacket::new(
                PacketType::AudioOpus,
                StreamId::PhoneToPc,
                (frame_idx + 1) as u16,
                (frame_idx as u32) * 10_000,
                echo_pcm16,
            );

            dsp.ingest_packet(pkt, 1);
            dsp.process_available(&cfg, &telemetry, &output_ring, &render_rx);

            let mut guard = output_ring.lock();
            assert_eq!(guard.ring.len(), FRAME_SAMPLES_10MS);
            if frame_idx >= CONVERGE_START {
                while let Some(out_s) = guard.ring.pop_front() {
                    output_energy_sq += (out_s as f64) * (out_s as f64);
                    measured_samples += 1;
                }
            } else {
                guard.ring.clear();
            }
        }

        let input_rms = (input_energy_sq / measured_samples as f64).sqrt();
        let output_rms = (output_energy_sq / measured_samples as f64).sqrt();
        assert!(input_rms > 0.05, "expected non-trivial echo input RMS, got {input_rms}");
        assert!(
            output_rms < input_rms * 0.6,
            "expected AEC3 echo attenuation (input_rms={input_rms:.4}, output_rms={output_rms:.4})"
        );
    }

    #[test]
    fn test_sonora_api() {
        use sonora::{AudioProcessing, Config, StreamConfig};
        use sonora::config::EchoCanceller;

        let config = Config {
            echo_canceller: Some(EchoCanceller::default()),
            ..Default::default()
        };
        let mut apm = AudioProcessing::builder()
            .config(config)
            .capture_config(StreamConfig::new(48000, 1))
            .render_config(StreamConfig::new(48000, 1))
            .build();

        let render_frame = [0.0f32; 480];
        let mut render_out = [0.0f32; 480];
        let capture_frame = [0.0f32; 480];
        let mut out_frame = [0.0f32; 480];

        apm.process_render_f32(&[&render_frame], &mut [&mut render_out]).expect("process render");
        apm.process_capture_f32(&[&capture_frame], &mut [&mut out_frame]).expect("process capture");
    }
}
