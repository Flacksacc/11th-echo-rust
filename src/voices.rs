//! Local, named Pocket TTS prompts. Original recordings are never retained here.
use candle_core::{DType, Tensor};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FOLLOW_DEFAULT: &str = "@settings-default";
const MAX_BYTES: u64 = 20_000_000;

#[derive(Clone, Serialize, Deserialize)]
pub struct Voice {
    pub id: String,
    pub name: String,
}
impl Voice {
    pub fn path(&self) -> PathBuf {
        directory().join(format!("{}.safetensors", self.id))
    }
    pub fn label(&self) -> String {
        self.name.clone()
    }
}
pub fn directory() -> PathBuf {
    if let Some(path) = std::env::var_os("ECHO_VOICE_LIBRARY_DIR") {
        return PathBuf::from(path);
    }
    crate::settings::settings_path().with_file_name("voice_library")
}
pub fn resolve(selection: &str, settings: &crate::settings::AppSettings) -> PathBuf {
    PathBuf::from(if selection == FOLLOW_DEFAULT {
        &settings.tts_voice_path
    } else {
        selection
    })
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
pub fn list() -> Result<Vec<Voice>, String> {
    let dir = directory();
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut voices = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_some_and(|e| e == "json") {
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            if bytes.len() > 4096 {
                return Err("Voice library metadata is invalid.".into());
            }
            let voice: Voice =
                serde_json::from_slice(&bytes).map_err(|_| "Voice library metadata is invalid.")?;
            if !valid_id(&voice.id) || path.file_stem().and_then(|s| s.to_str()) != Some(&voice.id)
            {
                return Err("Voice library metadata is invalid.".into());
            }
            if voice.path().is_file() {
                voices.push(voice);
            }
        }
    }
    voices.sort_by_key(|v| v.name.to_lowercase());
    Ok(voices)
}
fn save_metadata(voice: &Voice) -> Result<(), String> {
    let temp = directory().join(format!("{}.json.tmp", voice.id));
    std::fs::write(&temp, serde_json::to_vec(voice).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    std::fs::rename(temp, directory().join(format!("{}.json", voice.id))).map_err(|e| e.to_string())
}
fn clean_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if ["Alba", "Alba (standard)", "Use settings default"]
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
    {
        return Err("Choose a different name; Alba and the settings default are reserved.".into());
    }
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err("Enter a voice name of 1–80 characters.".into());
    }
    Ok(name.into())
}
pub fn rename(path: &Path, name: &str) -> Result<(), String> {
    let mut voice = list()?
        .into_iter()
        .find(|v| v.path() == path)
        .ok_or("Select a saved custom voice.")?;
    voice.name = clean_name(name)?;
    if list()?
        .iter()
        .any(|v| v.id != voice.id && v.name.eq_ignore_ascii_case(&voice.name))
    {
        return Err("A voice with that name already exists. Choose a different name.".into());
    }
    save_metadata(&voice)
}
pub fn delete(path: &Path) -> Result<(), String> {
    let voice = list()?
        .into_iter()
        .find(|v| v.path() == path)
        .ok_or("Select a saved custom voice.")?;
    std::fs::remove_file(voice.path()).map_err(|e| e.to_string())?;
    std::fs::remove_file(directory().join(format!("{}.json", voice.id))).map_err(|e| e.to_string())
}
pub fn export(source: &Path, target: &Path) -> Result<(), String> {
    if !list()?.iter().any(|voice| voice.path() == source) {
        return Err("Select a saved custom voice to export.".into());
    }
    std::fs::copy(source, target)
        .map(|_| ())
        .map_err(|e| e.to_string())
}
pub fn read_prompt(path: &Path, dim: usize) -> Result<Tensor, String> {
    let size = std::fs::metadata(path)
        .map_err(|_| "Voice prompt is missing.")?
        .len();
    if size == 0 || size > MAX_BYTES {
        return Err("Voice prompt must be smaller than 20 MB.".into());
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Voice prompt must be smaller than 20 MB.".into());
    }
    let tensors = candle_core::safetensors::load_buffer(&bytes, &candle_core::Device::Cpu)
        .map_err(|_| "Invalid safetensors voice prompt.")?;
    let prompt = tensors
        .get("audio_prompt")
        .ok_or("Voice prompt must contain audio_prompt.")?;
    let shape = prompt.dims();
    if shape.len() != 3
        || shape[0] != 1
        || shape[1] == 0
        || shape[1] > 1500
        || shape[2] != dim
        || prompt.dtype() != DType::F32
    {
        return Err("Voice prompt has an unsupported shape or data type.".into());
    }
    if prompt
        .flatten_all()
        .map_err(|e| e.to_string())?
        .to_vec1::<f32>()
        .map_err(|e| e.to_string())?
        .iter()
        .any(|v| !v.is_finite())
    {
        return Err("Voice prompt contains invalid values.".into());
    }
    Ok(prompt.clone())
}
pub fn import(source: &Path, name: &str) -> Result<Voice, String> {
    let name = clean_name(name)?;
    if list()?
        .iter()
        .any(|voice| voice.name.eq_ignore_ascii_case(&name))
    {
        return Err("A voice with that name already exists. Choose a different name.".into());
    }
    let size = std::fs::metadata(source).map_err(|e| e.to_string())?.len();
    if size == 0 || size > MAX_BYTES {
        return Err("Choose a WAV, MP3, or safetensors file smaller than 20 MB.".into());
    }
    let model = crate::assistant_voice::cached_model()?;
    let prompt = if source
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("safetensors"))
    {
        read_prompt(source, model.dim)?
    } else if source
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wav") || e.eq_ignore_ascii_case("mp3"))
    {
        encode_recording(&model, source)?
    } else {
        return Err(
            "Choose a WAV or MP3 recording, or a Pocket TTS safetensors voice prompt.".into(),
        );
    };
    // Verify the prompt against the selected model before committing any files.
    model
        .get_voice_state_from_prompt_tensor(&prompt)
        .map_err(|e| format!("Voice prompt could not load: {e}"))?;
    std::fs::create_dir_all(directory()).map_err(|e| e.to_string())?;
    let voice = Voice {
        id: crate::pipelines::new_id(),
        name,
    };
    let staged = voice.path().with_extension("safetensors.tmp");
    let result = (|| {
        candle_core::safetensors::save(
            &std::collections::HashMap::from([("audio_prompt".to_string(), prompt)]),
            &staged,
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(&staged, voice.path()).map_err(|e| e.to_string())?;
        save_metadata(&voice)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(staged);
        let _ = std::fs::remove_file(voice.path());
    }
    result?;
    Ok(voice)
}
fn read_mp3(path: &Path) -> anyhow::Result<(Tensor, u32)> {
    use rodio::Source;
    use std::io::Read;
    // Bound the input even if the source grows after import's metadata check.
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        !bytes.is_empty() && bytes.len() as u64 <= MAX_BYTES,
        "Choose an MP3 smaller than 20 MB."
    );
    let mut decoder = rodio::Decoder::new_mp3(std::io::Cursor::new(bytes))?;
    let rate = decoder.sample_rate().get();
    let channels = decoder.channels().get() as usize;
    anyhow::ensure!(
        rate > 0 && rate <= 192_000 && (1..=8).contains(&channels),
        "Unsupported MP3 sample rate or channels."
    );
    let limit = rate as usize * 30 * channels;
    let samples: Vec<f32> = decoder.by_ref().take(limit + 1).map(|v| v as f32).collect();
    anyhow::ensure!(
        !samples.is_empty() && samples.len() <= limit && samples.len() % channels == 0,
        "Use a nonempty MP3 recording of at most 30 seconds."
    );
    anyhow::ensure!(
        samples.iter().all(|v| v.is_finite()),
        "Recording contains invalid samples."
    );
    let mono: Vec<f32> = samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    let len = mono.len();
    Ok((
        Tensor::from_vec(mono, (1, len), &candle_core::Device::Cpu)?,
        rate,
    ))
}

fn encode_recording(model: &pocket_tts::TTSModel, path: &Path) -> Result<Tensor, String> {
    (|| -> anyhow::Result<Tensor> {
        let (audio, rate) = if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("mp3"))
        {
            read_mp3(path)?
        } else {
            let reader = hound::WavReader::open(path)?;
            let spec = reader.spec();
            if spec.sample_rate == 0
                || spec.channels == 0
                || spec.channels > 8
                || reader.duration() == 0
                || reader.duration() as u64 > spec.sample_rate as u64 * 30
                || ![8, 16, 24, 32].contains(&spec.bits_per_sample)
            {
                anyhow::bail!("Use a supported, nonempty WAV recording of at most 30 seconds.");
            }
            drop(reader);
            pocket_tts::audio::read_wav(path)?
        };
        if rate == 0 || audio.dim(1)? == 0 || audio.dim(1)? > rate as usize * 30 {
            anyhow::bail!("Use a nonempty voice recording of at most 30 seconds.");
        }
        let audio = audio.mean(0)?.unsqueeze(0)?;
        if audio
            .flatten_all()?
            .to_vec1::<f32>()?
            .iter()
            .any(|v| !v.is_finite())
        {
            anyhow::bail!("Recording contains invalid samples.");
        }
        let audio = if rate == model.sample_rate as u32 {
            audio
        } else {
            pocket_tts::audio::resample(&audio, rate, model.sample_rate as u32)?
        };
        let audio = audio.unsqueeze(0)?;
        let frame = model.mimi.frame_size();
        let samples = audio.dim(2)?;
        let padding = (frame - samples % frame) % frame;
        let audio = if padding == 0 {
            audio
        } else {
            Tensor::cat(
                &[
                    &audio,
                    &Tensor::zeros((1, 1, padding), audio.dtype(), &model.device)?,
                ],
                2,
            )?
        };
        let mut state = pocket_tts::voice_state::init_states(1, 1000);
        let mut chunks = Vec::new();
        for start in (0..audio.dim(2)?).step_by(frame * 120) {
            let len = (audio.dim(2)? - start).min(frame * 120);
            chunks.push(model.mimi.encode_to_latent(
                &audio.narrow(2, start, len)?,
                &mut state,
                0,
            )?);
        }
        let latents = Tensor::cat(&chunks, 2)?
            .transpose(1, 2)?
            .to_dtype(DType::F32)?;
        let (b, t, d) = latents.dims3()?;
        Ok(latents
            .reshape((b * t, d))?
            .matmul(&model.speaker_proj_weight.t()?)?
            .reshape((b, t, model.dim))?)
    })()
    .map_err(|e| format!("Voice recording could not be converted: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mp3_decodes_stereo_to_finite_mono_and_rejects_long_or_invalid_audio() {
        let (audio, rate) = read_mp3(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/voice-short.mp3"
        )))
        .unwrap();
        assert_eq!(rate, 24_000);
        assert_eq!(audio.dim(0).unwrap(), 1);
        assert!(audio.dim(1).unwrap() >= 6_000);
        let samples = audio.flatten_all().unwrap().to_vec1::<f32>().unwrap();
        assert!(samples.iter().all(|v| v.is_finite()));
        assert!(samples.iter().any(|v| v.abs() > 0.001));
        let error = read_mp3(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/voice-too-long.mp3"
        )))
        .unwrap_err();
        assert!(error.to_string().contains("at most 30 seconds"));
        assert!(read_mp3(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/Cargo.toml"
        )))
        .is_err());
    }

    #[test]
    fn default_resolution_preserves_explicit_alba_and_legacy_paths() {
        let settings = crate::settings::AppSettings {
            tts_voice_path: "saved.safetensors".into(),
            ..Default::default()
        };
        assert_eq!(
            resolve(FOLLOW_DEFAULT, &settings),
            PathBuf::from("saved.safetensors")
        );
        assert_eq!(resolve("", &settings), PathBuf::new());
        assert_eq!(
            resolve("legacy.wav", &settings),
            PathBuf::from("legacy.wav")
        );
        assert!(valid_id("18cf-42-1"));
        assert!(!valid_id("../outside"));
        assert!(clean_name("   ").is_err());
    }
    #[test]
    fn prompt_import_rejects_invalid_shape_dtype_and_nonfinite_data() {
        let path = std::env::temp_dir().join(format!(
            "echo-voice-prompt-{}.safetensors",
            crate::pipelines::new_id()
        ));
        for (shape, valid) in [
            ((1, 2, 4), true),
            ((2, 2, 4), false),
            ((1, 2, 5), false),
            ((1, 0, 4), false),
        ] {
            let tensor = Tensor::zeros(shape, DType::F32, &candle_core::Device::Cpu).unwrap();
            candle_core::safetensors::save(
                &std::collections::HashMap::from([("audio_prompt".to_string(), tensor)]),
                &path,
            )
            .unwrap();
            assert_eq!(read_prompt(&path, 4).is_ok(), valid);
        }
        let tensor =
            Tensor::from_vec(vec![f32::NAN; 4], (1, 1, 4), &candle_core::Device::Cpu).unwrap();
        candle_core::safetensors::save(
            &std::collections::HashMap::from([("audio_prompt".to_string(), tensor)]),
            &path,
        )
        .unwrap();
        assert!(read_prompt(&path, 4).is_err());
        let tensor = Tensor::zeros((1, 2, 4), DType::F64, &candle_core::Device::Cpu).unwrap();
        candle_core::safetensors::save(
            &std::collections::HashMap::from([("audio_prompt".to_string(), tensor)]),
            &path,
        )
        .unwrap();
        assert!(read_prompt(&path, 4).is_err());
        std::fs::write(&path, b"not a safetensors prompt").unwrap();
        assert!(read_prompt(&path, 4).is_err());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    #[ignore = "requires installed Pocket TTS; synthetic WAV conversion and streaming, no playback or network"]
    fn wav_prompt_roundtrip_and_speech_stream_smoke() {
        let root = std::env::temp_dir().join(format!(
            "echo-voice-roundtrip-{}",
            crate::pipelines::new_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let wav = root.join("synthetic.wav");
        let mut writer = hound::WavWriter::create(
            &wav,
            hound::WavSpec {
                channels: 1,
                sample_rate: 24000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for i in 0..24000 {
            writer
                .write_sample(
                    ((i as f32 * 440. * std::f32::consts::TAU / 24000.).sin() * 1500.) as i16,
                )
                .unwrap();
        }
        writer.finalize().unwrap();
        let model = crate::assistant_voice::cached_model().unwrap();
        let prompt = encode_recording(&model, &wav).unwrap();
        let target = root.join("voice.safetensors");
        candle_core::safetensors::save(
            &std::collections::HashMap::from([("audio_prompt".to_string(), prompt)]),
            &target,
        )
        .unwrap();
        let prompt = read_prompt(&target, model.dim).unwrap();
        let state = model.get_voice_state_from_prompt_tensor(&prompt).unwrap();
        let original = model.get_voice_state(&wav).unwrap();
        assert_eq!(state.len(), original.len());
        for (module, values) in &state {
            for (key, value) in values {
                let expected = &original[module][key];
                assert_eq!(value.dims(), expected.dims());
                let actual = value
                    .to_dtype(DType::F32)
                    .unwrap()
                    .flatten_all()
                    .unwrap()
                    .to_vec1::<f32>()
                    .unwrap();
                let expected = expected
                    .to_dtype(DType::F32)
                    .unwrap()
                    .flatten_all()
                    .unwrap()
                    .to_vec1::<f32>()
                    .unwrap();
                assert!(actual
                    .iter()
                    .zip(expected)
                    .all(|(a, b)| (a - b).abs() < 0.001));
            }
        }
        let mut samples = 0;
        for chunk in model.generate_stream("Hello. This is Echo.", &state) {
            samples += chunk.unwrap().elem_count();
        }
        assert!(samples > 0);
        // Exercise persistence, rename, export and delete in an isolated library.
        std::env::set_var("ECHO_VOICE_LIBRARY_DIR", root.join("library"));
        let voice = import(&wav, "Test voice").unwrap();
        assert!(voice
            .path()
            .extension()
            .is_some_and(|ext| ext == "safetensors"));
        assert!(import(&wav, "Test voice").is_err());
        rename(&voice.path(), "Renamed voice").unwrap();
        assert_eq!(list().unwrap()[0].name, "Renamed voice");
        let exported = root.join("exported.safetensors");
        export(&voice.path(), &exported).unwrap();
        let exported_prompt = read_prompt(&exported, model.dim).unwrap();
        assert_eq!(exported_prompt.dims(), prompt.dims());
        delete(&voice.path()).unwrap();
        assert!(list().unwrap().is_empty());
        std::env::remove_var("ECHO_VOICE_LIBRARY_DIR");
        std::fs::remove_dir_all(root).unwrap();
    }
}
