use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc::Sender; // Use bounded sender for backpressure
use std::error::Error;
use rubato::{Resampler, SincFixedIn, SincInterpolationType, SincInterpolationParameters, WindowFunction};
use std::sync::{Arc, Mutex};

/// Audio configuration constants
const TARGET_SAMPLE_RATE: u32 = 16000;
const CHUNK_SIZE: usize = 1024; // Process in chunks

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

    // Shared state for the callback (Resampler needs to be mutable)
    // In a real app, use a ring buffer. Here we keep it simple but thread-safe.
    let resampler_state = Arc::new(Mutex::new(resampler));
    // Buffer to hold incoming samples until we have enough for a resampler chunk
    let buffer_state = Arc::new(Mutex::new(Vec::<f32>::with_capacity(CHUNK_SIZE * 2)));
    
    let err_fn = move |err| eprintln!("❌ Audio stream error: {}", err);

    let sender_level = level_sender.clone();
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &_| {
                process_audio_f32(data, &sender, &sender_level, &resampler_state, &buffer_state, input_sample_rate);
            },
            err_fn,
            None 
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data: &[i16], _: &_| {
                // Convert i16 -> f32 for resampling
                let samples_f32: Vec<f32> = data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                process_audio_f32(&samples_f32, &sender, &sender_level, &resampler_state, &buffer_state, input_sample_rate);
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
        // If we have enough data for the resampler
        let chunks_needed = buffer.len() / CHUNK_SIZE;
        if chunks_needed > 0 {
             // Basic implementation: take slices.
             // Rubato requires strict chunk sizes for SincFixedIn
             let input_frames = vec![buffer.drain(0..CHUNK_SIZE).collect::<Vec<f32>>()];
             
             if let Ok(output_frames) = resampler.process(&input_frames, None) {
                 if let Some(channel_data) = output_frames.first() {
                     let output_i16: Vec<i16> = channel_data.iter()
                        .map(|&s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                        .collect();
                     let _ = sender.blocking_send(output_i16);
                 }
             }
        }
    } else {
        // No resampling needed
        let output_i16: Vec<i16> = buffer.drain(..)
            .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();
        let _ = sender.blocking_send(output_i16);
    }
}
