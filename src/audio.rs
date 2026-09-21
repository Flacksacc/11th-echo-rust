use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::collections::VecDeque;
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::thread;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::{self, Receiver, Sender, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// Audio configuration constants
const TARGET_SAMPLE_RATE: u32 = 16000;
const CHUNK_SIZE: usize = TARGET_SAMPLE_RATE as usize; // Send 1 second chunks at 16kHz mono
const RESAMPLER_INPUT_CHUNK: usize = 1600;
const PRECONNECT_BUFFER_SAMPLES: usize = TARGET_SAMPLE_RATE as usize * 5; // Keep last 5s before consumer catches up
const WARM_PRE_ROLL_SAMPLES: usize = TARGET_SAMPLE_RATE as usize / 2;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CaptureFlushStats {
    pub pending_input_samples: usize,
    pub flushed_output_samples: usize,
}

pub struct AudioCapture {
    stream: Option<cpal::Stream>,
    worker: Option<thread::JoinHandle<CaptureFlushStats>>,
    device_name: String,
}

impl AudioCapture {
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Stops the hardware callback. The returned worker must be joined before
    /// the transcription provider is asked to commit so queued and partial
    /// resampler input reaches the provider first.
    pub fn begin_shutdown(mut self) -> thread::JoinHandle<CaptureFlushStats> {
        self.stream.take();
        self.worker
            .take()
            .expect("audio capture worker is present until shutdown")
    }
}

enum WarmCaptureCommand {
    Attach {
        epoch: u64,
        sender: Sender<Vec<i16>>,
        response: oneshot::Sender<Result<usize, String>>,
    },
    Detach {
        epoch: u64,
        response: oneshot::Sender<Result<(), String>>,
    },
}

/// A shared-mode input stream that stays open while Echo is idle. Only the
/// newest 500 ms are retained, and no audio leaves the router until a session
/// explicitly attaches.
pub struct WarmAudioCapture {
    capture: Option<AudioCapture>,
    command_tx: UnboundedSender<WarmCaptureCommand>,
    router_task: Option<JoinHandle<()>>,
    device_name: String,
}

impl WarmAudioCapture {
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub async fn attach(&self, epoch: u64, sender: Sender<Vec<i16>>) -> Result<usize, String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(WarmCaptureCommand::Attach {
                epoch,
                sender,
                response: response_tx,
            })
            .map_err(|_| "The warm microphone worker is unavailable".to_string())?;
        response_rx
            .await
            .map_err(|_| "The warm microphone worker stopped unexpectedly".to_string())?
    }

    pub async fn detach(&self, epoch: u64) -> Result<(), String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(WarmCaptureCommand::Detach {
                epoch,
                response: response_tx,
            })
            .map_err(|_| "The warm microphone worker is unavailable".to_string())?;
        response_rx
            .await
            .map_err(|_| "The warm microphone worker stopped unexpectedly".to_string())?
    }

    pub async fn shutdown(mut self) -> Result<CaptureFlushStats, String> {
        let capture = self
            .capture
            .take()
            .ok_or_else(|| "The warm microphone stream was already stopped".to_string())?;
        let worker = capture.begin_shutdown();
        let stats = tokio::task::spawn_blocking(move || {
            worker
                .join()
                .map_err(|_| "Warm microphone conversion worker panicked".to_string())
        })
        .await
        .map_err(|err| err.to_string())??;
        drop(self.command_tx);
        if let Some(router_task) = self.router_task.take() {
            router_task.await.map_err(|err| err.to_string())?;
        }
        Ok(stats)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputDeviceSnapshot {
    pub devices: Vec<String>,
    pub default_device: Option<String>,
}

pub fn input_device_snapshot() -> InputDeviceSnapshot {
    let host = cpal::default_host();
    let mut names = Vec::new();

    if let Ok(devices) = host.input_devices() {
        for device in devices {
            if let Ok(name) = device.name() {
                names.push(name);
            }
        }
    }

    let default_device = host
        .default_input_device()
        .and_then(|device| device.name().ok())
        .filter(|name| !name.trim().is_empty());
    if let Some(default_name) = default_device.as_ref() {
        if !names.iter().any(|name| name == default_name) {
            names.push(default_name.clone());
        }
    }

    names.sort();
    names.dedup();
    InputDeviceSnapshot {
        devices: names,
        default_device,
    }
}

struct CircularSampleBuffer {
    samples: VecDeque<i16>,
    capacity: usize,
}

impl CircularSampleBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn push_samples(&mut self, incoming: &[i16]) {
        if incoming.is_empty() {
            return;
        }

        if incoming.len() >= self.capacity {
            self.clear();
            self.samples
                .extend(incoming[incoming.len() - self.capacity..].iter().copied());
            return;
        }

        let overflow = self
            .samples
            .len()
            .saturating_add(incoming.len())
            .saturating_sub(self.capacity);

        if overflow > 0 {
            for sample in self.samples.iter_mut().take(overflow) {
                *sample = 0;
            }
            self.samples.drain(0..overflow);
        }

        self.samples.extend(incoming.iter().copied());
    }

    fn pop_chunk(&mut self, max_len: usize) -> Option<Vec<i16>> {
        if self.samples.is_empty() {
            return None;
        }

        let len = self.samples.len().min(max_len);
        Some(self.samples.drain(0..len).collect())
    }

    fn push_front_samples(&mut self, chunk: &[i16]) {
        for sample in chunk.iter().rev() {
            self.samples.push_front(*sample);
        }
    }

    fn clear(&mut self) {
        for sample in &mut self.samples {
            *sample = 0;
        }
        self.samples.clear();
    }

    fn snapshot(&self) -> Vec<i16> {
        self.samples.iter().copied().collect()
    }
}

pub fn start_warm_audio_capture(
    level_sender: Sender<f32>,
    preferred_device_name: Option<String>,
) -> Result<WarmAudioCapture, Box<dyn Error + Send + Sync>> {
    let (audio_tx, audio_rx) = mpsc::channel::<Vec<i16>>(50);
    let (raw_level_tx, raw_level_rx) = mpsc::channel::<f32>(10);
    let capture = start_audio_capture(audio_tx, raw_level_tx, preferred_device_name)?;
    let device_name = capture.device_name().to_string();
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let router_task = tokio::spawn(run_warm_audio_router(
        audio_rx,
        raw_level_rx,
        level_sender,
        command_rx,
    ));
    Ok(WarmAudioCapture {
        capture: Some(capture),
        command_tx,
        router_task: Some(router_task),
        device_name,
    })
}

async fn route_warm_chunk(
    pre_roll: &mut CircularSampleBuffer,
    active: &mut Option<(u64, Sender<Vec<i16>>)>,
    chunk: Vec<i16>,
) {
    pre_roll.push_samples(&chunk);
    if let Some((_, sender)) = active.as_ref() {
        if sender.send(chunk).await.is_err() {
            *active = None;
            pre_roll.clear();
        }
    }
}

async fn run_warm_audio_router(
    mut audio_rx: Receiver<Vec<i16>>,
    mut raw_level_rx: Receiver<f32>,
    level_sender: Sender<f32>,
    mut command_rx: UnboundedReceiver<WarmCaptureCommand>,
) {
    let mut pre_roll = CircularSampleBuffer::new(WARM_PRE_ROLL_SAMPLES);
    let mut active: Option<(u64, Sender<Vec<i16>>)> = None;
    let mut level_channel_open = true;

    loop {
        tokio::select! {
            biased;
            command = command_rx.recv() => {
                let Some(command) = command else { break; };
                match command {
                    WarmCaptureCommand::Attach { epoch, sender, response } => {
                        if active.is_some() {
                            let _ = response.send(Err("The warm microphone is already attached to a session".to_string()));
                            continue;
                        }
                        // Include converted samples that reached the router before
                        // the start command, even when both channels became ready
                        // during the same scheduler turn.
                        while let Ok(chunk) = audio_rx.try_recv() {
                            route_warm_chunk(&mut pre_roll, &mut active, chunk).await;
                        }
                        while raw_level_rx.try_recv().is_ok() {}
                        let buffered = pre_roll.snapshot();
                        let buffered_samples = buffered.len();
                        if !buffered.is_empty() && sender.send(buffered).await.is_err() {
                            let _ = response.send(Err("The transcription audio channel closed before warm pre-roll was delivered".to_string()));
                            continue;
                        }
                        active = Some((epoch, sender));
                        let _ = response.send(Ok(buffered_samples));
                    }
                    WarmCaptureCommand::Detach { epoch, response } => {
                        while let Ok(chunk) = audio_rx.try_recv() {
                            route_warm_chunk(&mut pre_roll, &mut active, chunk).await;
                        }
                        let result = match active.as_ref() {
                            Some((active_epoch, _)) if *active_epoch == epoch => {
                                active = None;
                                pre_roll.clear();
                                Ok(())
                            }
                            Some((active_epoch, _)) => Err(format!(
                                "Warm microphone session mismatch: active epoch {active_epoch}, requested {epoch}"
                            )),
                            None => Err("The warm microphone was not attached to a session".to_string()),
                        };
                        let _ = response.send(result);
                    }
                }
            }
            chunk = audio_rx.recv() => {
                let Some(chunk) = chunk else { break; };
                route_warm_chunk(&mut pre_roll, &mut active, chunk).await;
            }
            level = raw_level_rx.recv(), if level_channel_open => {
                match level {
                    Some(level) if active.is_some() => {
                        let _ = level_sender.try_send(level);
                    }
                    Some(_) => {}
                    None => level_channel_open = false,
                }
            }
        }
    }
}

/// Starts the audio recording stream.
/// Audio chunks (raw i16 PCM @ 16kHz) are sent to the provided `sender`.
pub fn start_audio_capture(
    sender: Sender<Vec<i16>>,
    level_sender: Sender<f32>,
    preferred_device_name: Option<String>,
) -> Result<AudioCapture, Box<dyn Error + Send + Sync>> {
    let host = cpal::default_host();
    let device = if let Some(name) = preferred_device_name {
        if name.trim().is_empty() {
            host.default_input_device()
                .ok_or("No input device available")?
        } else if let Ok(mut devices) = host.input_devices() {
            devices
                .find(|d| d.name().map(|n| n == name).unwrap_or(false))
                .or_else(|| host.default_input_device())
                .ok_or("No input device available")?
        } else {
            host.default_input_device()
                .ok_or("No input device available")?
        }
    } else {
        host.default_input_device()
            .ok_or("No input device available")?
    };
    let config = device.default_input_config()?;
    let input_sample_rate = config.sample_rate().0;
    let input_channels = config.channels() as usize;

    let device_name = device.name().unwrap_or_default();
    crate::echo_info!(
        "audio",
        "Input device={} sample_rate_hz={} channels={}",
        device_name,
        input_sample_rate,
        input_channels
    );

    // Setup Resampler if needed
    let resampler = if input_sample_rate != TARGET_SAMPLE_RATE {
        crate::echo_info!(
            "audio",
            "Resampling input_hz={} target_hz={}",
            input_sample_rate,
            TARGET_SAMPLE_RATE
        );

        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };

        SincFixedIn::<f32>::new(
            TARGET_SAMPLE_RATE as f64 / input_sample_rate as f64,
            2.0, // Max ratio
            params,
            RESAMPLER_INPUT_CHUNK,
            1, // channels
        )
        .ok()
    } else {
        None
    };

    // Shared state for the callback (Resampler needs to be mutable).
    let resampler_state = Arc::new(Mutex::new(resampler));
    // Buffer to hold incoming samples until we have enough for a resampler chunk
    let buffer_state = Arc::new(Mutex::new(Vec::<f32>::with_capacity(CHUNK_SIZE * 2)));
    let ring_buffer_state = Arc::new(Mutex::new(CircularSampleBuffer::new(
        PRECONNECT_BUFFER_SAMPLES,
    )));

    let err_fn = move |err| crate::echo_error!("audio", "Capture stream error: {}", err);
    let (raw_tx, raw_rx) = crossbeam_channel::bounded::<Vec<f32>>(32);
    let worker_rx = raw_rx.clone();
    let worker_sender = sender.clone();
    let worker = thread::spawn(move || {
        while let Ok(raw) = worker_rx.recv() {
            let mono = if input_channels <= 1 {
                raw
            } else {
                raw.chunks(input_channels)
                    .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
                    .collect()
            };
            process_audio_f32(
                &mono,
                &worker_sender,
                &level_sender,
                &resampler_state,
                &buffer_state,
                &ring_buffer_state,
                input_sample_rate,
            );
        }
        flush_pending_audio(
            &worker_sender,
            &resampler_state,
            &buffer_state,
            &ring_buffer_state,
        )
    });

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            let drop_rx = raw_rx.clone();
            device.build_input_stream(
                &config.into(),
                move |data: &[f32], _: &_| enqueue_raw_audio(&raw_tx, &drop_rx, data.to_vec()),
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::I16 => {
            let drop_rx = raw_rx.clone();
            device.build_input_stream(
                &config.into(),
                move |data: &[i16], _: &_| {
                    let samples = data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                    enqueue_raw_audio(&raw_tx, &drop_rx, samples);
                },
                err_fn,
                None,
            )?
        }
        _ => return Err("Unsupported sample format".into()),
    };

    stream.play()?;
    Ok(AudioCapture {
        stream: Some(stream),
        worker: Some(worker),
        device_name,
    })
}

fn enqueue_raw_audio(
    sender: &crossbeam_channel::Sender<Vec<f32>>,
    drop_receiver: &crossbeam_channel::Receiver<Vec<f32>>,
    samples: Vec<f32>,
) {
    match sender.try_send(samples) {
        Ok(()) => {}
        Err(crossbeam_channel::TrySendError::Full(samples)) => {
            let _ = drop_receiver.try_recv();
            let _ = sender.try_send(samples);
        }
        Err(crossbeam_channel::TrySendError::Disconnected(_)) => {}
    }
}

fn process_audio_f32(
    input: &[f32],
    sender: &Sender<Vec<i16>>,
    level_sender: &Sender<f32>,
    resampler_state: &Arc<Mutex<Option<SincFixedIn<f32>>>>,
    buffer_state: &Arc<Mutex<Vec<f32>>>,
    ring_buffer_state: &Arc<Mutex<CircularSampleBuffer>>,
    _input_rate: u32,
) {
    // Calculate peak level for feedback
    let mut peak = 0.0f32;
    for &sample in input {
        let abs = sample.abs();
        if abs > peak {
            peak = abs;
        }
    }
    let _ = level_sender.try_send(peak);

    let mut buffer = buffer_state.lock().unwrap();
    buffer.extend_from_slice(input);

    let mut resampler_guard = resampler_state.lock().unwrap();

    if let Some(resampler) = resampler_guard.as_mut() {
        while buffer.len() >= RESAMPLER_INPUT_CHUNK {
            // Rubato requires strict chunk sizes for SincFixedIn
            let input_frames = vec![buffer.drain(0..RESAMPLER_INPUT_CHUNK).collect::<Vec<f32>>()];

            if let Ok(output_frames) = resampler.process(&input_frames, None) {
                if let Some(channel_data) = output_frames.first() {
                    let output_i16: Vec<i16> = channel_data
                        .iter()
                        .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                        .collect();
                    enqueue_and_flush(sender, ring_buffer_state, output_i16);
                }
            }
        }
    } else {
        // No resampling needed
        let output_i16: Vec<i16> = buffer
            .drain(..)
            .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();
        enqueue_and_flush(sender, ring_buffer_state, output_i16);
    }
}

fn enqueue_and_flush(
    sender: &Sender<Vec<i16>>,
    ring_buffer_state: &Arc<Mutex<CircularSampleBuffer>>,
    samples: Vec<i16>,
) {
    let mut ring_buffer = ring_buffer_state.lock().unwrap();
    ring_buffer.push_samples(&samples);

    while let Some(chunk) = ring_buffer.pop_chunk(CHUNK_SIZE) {
        match sender.try_send(chunk) {
            Ok(()) => {}
            Err(TrySendError::Full(chunk)) => {
                ring_buffer.push_front_samples(&chunk);
                break;
            }
            Err(TrySendError::Closed(_)) => {
                ring_buffer.clear();
                break;
            }
        }
    }
}

fn flush_pending_audio(
    sender: &Sender<Vec<i16>>,
    resampler_state: &Arc<Mutex<Option<SincFixedIn<f32>>>>,
    buffer_state: &Arc<Mutex<Vec<f32>>>,
    ring_buffer_state: &Arc<Mutex<CircularSampleBuffer>>,
) -> CaptureFlushStats {
    let mut stats = CaptureFlushStats::default();
    let mut buffer = buffer_state.lock().unwrap();
    stats.pending_input_samples = buffer.len();

    let mut resampler_guard = resampler_state.lock().unwrap();
    if let Some(resampler) = resampler_guard.as_mut() {
        if !buffer.is_empty() {
            let input_frames = vec![buffer.drain(..).collect::<Vec<f32>>()];
            if let Ok(output_frames) = resampler.process_partial(Some(&input_frames), None) {
                if let Some(channel_data) = output_frames.first() {
                    let output_i16: Vec<i16> = channel_data
                        .iter()
                        .map(|&sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                        .collect();
                    stats.flushed_output_samples = output_i16.len();
                    enqueue_and_flush(sender, ring_buffer_state, output_i16);
                }
            }
        }
    } else if !buffer.is_empty() {
        let output_i16: Vec<i16> = buffer
            .drain(..)
            .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();
        stats.flushed_output_samples = output_i16.len();
        enqueue_and_flush(sender, ring_buffer_state, output_i16);
    }
    drop(resampler_guard);
    drop(buffer);

    // Normal capture uses try_send so the realtime conversion worker never
    // blocks. Shutdown is different: wait for the downstream forwarding task
    // to accept every retained sample before declaring capture complete.
    loop {
        let next = {
            let mut ring_buffer = ring_buffer_state.lock().unwrap();
            ring_buffer.pop_chunk(CHUNK_SIZE)
        };
        let Some(chunk) = next else {
            break;
        };
        if sender.blocking_send(chunk).is_err() {
            break;
        }
    }

    stats
}

#[cfg(test)]
mod tests {
    use super::{
        enqueue_and_flush, flush_pending_audio, run_warm_audio_router, CircularSampleBuffer,
        WarmCaptureCommand, CHUNK_SIZE, TARGET_SAMPLE_RATE, WARM_PRE_ROLL_SAMPLES,
    };
    use rubato::{SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
    use std::sync::{Arc, Mutex};
    use tokio::sync::{mpsc, oneshot};

    #[test]
    fn circular_buffer_trims_to_capacity() {
        let mut b = CircularSampleBuffer::new(4);
        b.push_samples(&[1, 2, 3, 4, 5, 6]);
        let out = b.pop_chunk(10).unwrap();
        assert_eq!(out, vec![3, 4, 5, 6]);
    }

    #[test]
    fn circular_buffer_push_front_restores_order() {
        let mut b = CircularSampleBuffer::new(10);
        b.push_samples(&[1, 2, 3]);
        let chunk = b.pop_chunk(2).unwrap();
        assert_eq!(chunk, vec![1, 2]);
        b.push_front_samples(&chunk);
        let out = b.pop_chunk(10).unwrap();
        assert_eq!(out, vec![1, 2, 3]);
    }

    #[test]
    fn circular_buffer_clear_empties_storage() {
        let mut b = CircularSampleBuffer::new(10);
        b.push_samples(&[1, 2, 3]);
        b.clear();
        assert!(b.pop_chunk(10).is_none());
    }

    #[tokio::test]
    async fn enqueue_and_flush_sends_when_channel_has_space() {
        let (tx, mut rx) = mpsc::channel::<Vec<i16>>(4);
        let ring = Arc::new(Mutex::new(CircularSampleBuffer::new(CHUNK_SIZE * 2)));
        enqueue_and_flush(&tx, &ring, vec![1; CHUNK_SIZE]);
        let got = rx.recv().await.unwrap();
        assert_eq!(got.len(), CHUNK_SIZE);
    }

    #[tokio::test]
    async fn enqueue_and_flush_handles_full_channel() {
        let (tx, mut rx) = mpsc::channel::<Vec<i16>>(1);
        let ring = Arc::new(Mutex::new(CircularSampleBuffer::new(CHUNK_SIZE * 3)));

        // Fill channel so next send hits TrySendError::Full.
        tx.try_send(vec![9; CHUNK_SIZE]).unwrap();
        enqueue_and_flush(&tx, &ring, vec![1; CHUNK_SIZE]);

        // First message is the pre-filled one.
        let _ = rx.recv().await.unwrap();

        // The chunk should have been preserved in ring buffer.
        let mut rb = ring.lock().unwrap();
        let preserved = rb.pop_chunk(CHUNK_SIZE).unwrap();
        assert_eq!(preserved.len(), CHUNK_SIZE);
    }

    #[tokio::test]
    async fn enqueue_and_flush_handles_closed_channel() {
        let (tx, rx) = mpsc::channel::<Vec<i16>>(1);
        drop(rx); // force TrySendError::Closed
        let ring = Arc::new(Mutex::new(CircularSampleBuffer::new(CHUNK_SIZE * 2)));
        enqueue_and_flush(&tx, &ring, vec![1; CHUNK_SIZE]);
        let mut rb = ring.lock().unwrap();
        assert!(rb.pop_chunk(CHUNK_SIZE).is_none());
    }

    #[tokio::test]
    async fn shutdown_flushes_a_partial_resampler_block() {
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };
        let resampler = SincFixedIn::<f32>::new(
            TARGET_SAMPLE_RATE as f64 / 48_000.0,
            2.0,
            params,
            CHUNK_SIZE,
            1,
        )
        .unwrap();
        let resampler = Arc::new(Mutex::new(Some(resampler)));
        let pending = Arc::new(Mutex::new(vec![0.25; CHUNK_SIZE / 4]));
        let ring = Arc::new(Mutex::new(CircularSampleBuffer::new(CHUNK_SIZE)));
        let (tx, mut rx) = mpsc::channel::<Vec<i16>>(2);

        let stats = flush_pending_audio(&tx, &resampler, &pending, &ring);

        assert_eq!(stats.pending_input_samples, CHUNK_SIZE / 4);
        assert!(stats.flushed_output_samples > 0);
        assert!(pending.lock().unwrap().is_empty());
        assert_eq!(rx.recv().await.unwrap().len(), stats.flushed_output_samples);
    }

    #[tokio::test]
    async fn warm_router_prepends_only_the_latest_half_second_then_forwards_live_audio() {
        let (audio_tx, audio_rx) = mpsc::channel(8);
        let (raw_level_tx, raw_level_rx) = mpsc::channel(2);
        let (level_tx, _level_rx) = mpsc::channel(2);
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_warm_audio_router(
            audio_rx,
            raw_level_rx,
            level_tx,
            command_rx,
        ));

        let idle: Vec<i16> = (0..10_000).map(|sample| sample as i16).collect();
        audio_tx.send(idle.clone()).await.unwrap();

        let (session_tx, mut session_rx) = mpsc::channel(8);
        let (attached_tx, attached_rx) = oneshot::channel();
        command_tx
            .send(WarmCaptureCommand::Attach {
                epoch: 7,
                sender: session_tx,
                response: attached_tx,
            })
            .unwrap();
        assert_eq!(attached_rx.await.unwrap().unwrap(), WARM_PRE_ROLL_SAMPLES);
        assert_eq!(
            session_rx.recv().await.unwrap(),
            idle[idle.len() - WARM_PRE_ROLL_SAMPLES..]
        );

        audio_tx.send(vec![11, 12, 13]).await.unwrap();
        assert_eq!(session_rx.recv().await.unwrap(), vec![11, 12, 13]);

        let (detached_tx, detached_rx) = oneshot::channel();
        command_tx
            .send(WarmCaptureCommand::Detach {
                epoch: 7,
                response: detached_tx,
            })
            .unwrap();
        detached_rx.await.unwrap().unwrap();
        assert_eq!(session_rx.recv().await, None);

        drop(raw_level_tx);
        drop(audio_tx);
        drop(command_tx);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn warm_router_suppresses_idle_levels_and_clears_audio_between_sessions() {
        let (audio_tx, audio_rx) = mpsc::channel(8);
        let (raw_level_tx, raw_level_rx) = mpsc::channel(8);
        let (level_tx, mut level_rx) = mpsc::channel(8);
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_warm_audio_router(
            audio_rx,
            raw_level_rx,
            level_tx,
            command_rx,
        ));

        raw_level_tx.send(0.25).await.unwrap();
        audio_tx.send(vec![1, 2, 3]).await.unwrap();

        let (first_session_tx, _first_session_rx) = mpsc::channel(8);
        let (attached_tx, attached_rx) = oneshot::channel();
        command_tx
            .send(WarmCaptureCommand::Attach {
                epoch: 1,
                sender: first_session_tx,
                response: attached_tx,
            })
            .unwrap();
        attached_rx.await.unwrap().unwrap();
        assert!(level_rx.try_recv().is_err());

        raw_level_tx.send(0.5).await.unwrap();
        assert_eq!(level_rx.recv().await, Some(0.5));

        let (detached_tx, detached_rx) = oneshot::channel();
        command_tx
            .send(WarmCaptureCommand::Detach {
                epoch: 1,
                response: detached_tx,
            })
            .unwrap();
        detached_rx.await.unwrap().unwrap();

        audio_tx.send(vec![8, 9]).await.unwrap();
        let (second_session_tx, mut second_session_rx) = mpsc::channel(8);
        let (second_attached_tx, second_attached_rx) = oneshot::channel();
        command_tx
            .send(WarmCaptureCommand::Attach {
                epoch: 2,
                sender: second_session_tx,
                response: second_attached_tx,
            })
            .unwrap();
        assert_eq!(second_attached_rx.await.unwrap().unwrap(), 2);
        assert_eq!(second_session_rx.recv().await, Some(vec![8, 9]));

        drop(second_session_rx);
        drop(raw_level_tx);
        drop(audio_tx);
        drop(command_tx);
        task.await.unwrap();
    }
}
