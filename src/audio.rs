use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc::Sender; // Use bounded sender for backpressure
use tokio::sync::mpsc::error::TrySendError;
use std::error::Error;
use rubato::{Resampler, SincFixedIn, SincInterpolationType, SincInterpolationParameters, WindowFunction};
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;

/// Audio configuration constants
const TARGET_SAMPLE_RATE: u32 = 16000;
const CHUNK_SIZE: usize = 1024; // Process in chunks
const PRECONNECT_BUFFER_SAMPLES: usize = TARGET_SAMPLE_RATE as usize * 5; // Keep last 5s before consumer catches up

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
            self.samples.clear();
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
        self.samples.clear();
    }
}

/// Starts the audio recording stream.
/// Audio chunks (raw i16 PCM @ 16kHz) are sent to the provided `sender`.
pub fn start_audio_capture(
    sender: Sender<Vec<i16>>,
    level_sender: Sender<f32>,
) -> Result<cpal::Stream, Box<dyn Error + Send + Sync>> {
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or("No input device available")?;
    let config = device.default_input_config()?;
    let input_sample_rate = config.sample_rate().0;
    
    println!("🎤 Input device: {} @ {}Hz", device.name().unwrap_or_default(), input_sample_rate);

    // Setup Resampler if needed
    let resampler = if input_sample_rate != TARGET_SAMPLE_RATE {
        println!("🔄 Resampling from {}Hz to {}Hz", input_sample_rate, TARGET_SAMPLE_RATE);
        
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
            CHUNK_SIZE, 
            1 // channels
        ).ok()
    } else {
        None
    };

    // Shared state for the callback (Resampler needs to be mutable).
    let resampler_state = Arc::new(Mutex::new(resampler));
    // Buffer to hold incoming samples until we have enough for a resampler chunk
    let buffer_state = Arc::new(Mutex::new(Vec::<f32>::with_capacity(CHUNK_SIZE * 2)));
    let ring_buffer_state = Arc::new(Mutex::new(CircularSampleBuffer::new(PRECONNECT_BUFFER_SAMPLES)));
    
    let err_fn = move |err| eprintln!("❌ Audio stream error: {}", err);

    let sender_level = level_sender.clone();
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &_| {
                process_audio_f32(
                    data,
                    &sender,
                    &sender_level,
                    &resampler_state,
                    &buffer_state,
                    &ring_buffer_state,
                    input_sample_rate
                );
            },
            err_fn,
            None 
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data: &[i16], _: &_| {
                // Convert i16 -> f32 for resampling
                let samples_f32: Vec<f32> = data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                process_audio_f32(
                    &samples_f32,
                    &sender,
                    &sender_level,
                    &resampler_state,
                    &buffer_state,
                    &ring_buffer_state,
                    input_sample_rate
                );
            },
            err_fn,
            None
        )?,
        _ => return Err("Unsupported sample format".into()),
    };

    stream.play()?;
    Ok(stream)
}

fn process_audio_f32(
    input: &[f32], 
    sender: &Sender<Vec<i16>>, 
    level_sender: &Sender<f32>,
    resampler_state: &Arc<Mutex<Option<SincFixedIn<f32>>>>,
    buffer_state: &Arc<Mutex<Vec<f32>>>,
    ring_buffer_state: &Arc<Mutex<CircularSampleBuffer>>,
    _input_rate: u32
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
        while buffer.len() >= CHUNK_SIZE {
            // Rubato requires strict chunk sizes for SincFixedIn
            let input_frames = vec![buffer.drain(0..CHUNK_SIZE).collect::<Vec<f32>>()];

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
