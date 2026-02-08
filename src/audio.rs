use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::mpsc::Sender;
use std::error::Error;

/// Audio configuration constants
const TARGET_SAMPLE_RATE: u32 = 16000;
const TARGET_CHANNELS: u16 = 1;

/// Starts the audio recording stream.
/// Audio chunks (raw i16 PCM) are sent to the provided `sender`.
pub fn start_audio_capture(sender: Sender<Vec<i16>>) -> Result<cpal::Stream, Box<dyn Error + Send + Sync>> {
    let host = cpal::default_host();
    
    // Get the default input device
    let device = host.default_input_device()
        .ok_or("No input device available")?;
        
    println!("🎤 Input device: {}", device.name().unwrap_or_else(|_| "Unknown".to_string()));

    // Try to find a config supported by the device
    let config = device.default_input_config()?;
    
    println!("🎧 Default config: {:?} channels, {} Hz", config.channels(), config.sample_rate().0);

    // For this MVP, we are somewhat relying on the device supporting standard formats.
    // In a production app, we would add a proper Resampler here to ensure 16kHz output.
    // For now, we just pass the raw samples and let the consumer handle (or fail) if mismatched.
    // TODO: Implement `samplerate` crate for high-quality resampling to 16000Hz.

    let err_fn = move |err| {
        eprintln!("❌ an error occurred on stream: {}", err);
    };

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &_| {
                // Convert f32 to i16 for ElevenLabs
                let samples: Vec<i16> = data.iter()
                    .map(|&s| (s * i16::MAX as f32) as i16)
                    .collect();
                if let Err(_) = sender.send(samples) {
                    // Channel closed, stop streaming?
                }
            },
            err_fn,
            None 
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data: &[i16], _: &_| {
                if let Err(_) = sender.send(data.to_vec()) {
                    // Channel closed
                }
            },
            err_fn,
            None
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config.into(),
            move |data: &[u16], _: &_| {
                // Convert u16 to i16
                let samples: Vec<i16> = data.iter()
                    .map(|&s| (s as i32 - 32768) as i16)
                    .collect();
                if let Err(_) = sender.send(samples) {
                    // Channel closed
                }
            },
            err_fn,
            None
        )?,
        _ => return Err("Unsupported sample format".into()),
    };

    stream.play()?;
    Ok(stream)
}
