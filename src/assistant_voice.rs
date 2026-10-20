//! Pocket TTS assets and playback live outside the dictation pipeline.
//! All downloaded assets are pinned to a model revision and checked before use.
use futures_util::StreamExt;
use rodio::cpal::traits::{DeviceTrait, HostTrait};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering},
    Arc, LazyLock, Mutex,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CLONE_REVISION: &str = "427e3d61b276ed69fdd03de0d185fa8a8d97fc5b";
const PUBLIC_REVISION: &str = "d4fdd22ae8c8e1cb3634e150ebeff1dab2d16df3";
const TOKENIZER_SHA256: &str = "d461765ae179566678c93091c5fa6f2984c31bbe990bf1aa62d92c64d91bc3f6";
const TOKENIZER_NAME: &str = "tokenizer.model";
const WEIGHTS_NAME: &str = "tts_b6369a24.safetensors";
const DEFAULT_VOICE: &[u8] = include_bytes!("../assets/pocket-tts-alba.safetensors");
const MAX_ASSET_BYTES: u64 = 1_000_000_000;
static PLAYBACK_GENERATION: AtomicU64 = AtomicU64::new(0);
static PLAYBACK_SPEED_BITS: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static OUTPUT_VOLUME_BITS: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static OUTPUT_DEVICE: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));
static ACTIVE_SINK: LazyLock<Mutex<Option<Arc<rodio::Player>>>> =
    LazyLock::new(|| Mutex::new(None));
static CACHED_MODEL: LazyLock<ModelCache<pocket_tts::TTSModel>> =
    LazyLock::new(ModelCache::default);
static TTS_INFERENCE: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static SENTENCE_LANE: LazyLock<crate::speech_lane::Lane> =
    LazyLock::new(crate::speech_lane::Lane::default);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelReadiness {
    Unloaded,
    Loading,
    Ready,
    Failed,
}

impl ModelReadiness {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unloaded => "",
            Self::Loading => "Loading Pocket TTS weights… Speech is not ready yet.",
            Self::Ready => "Pocket TTS ready",
            Self::Failed => "Pocket TTS could not load. Speech is not ready.",
        }
    }
}

// Readiness remains readable without waiting for the weights-loading lock.
// The same cache is used by startup preloading, voice previews, and graph speech.
struct ModelCache<T> {
    model: Mutex<Option<Arc<T>>>,
    state: AtomicU8,
}

impl<T> Default for ModelCache<T> {
    fn default() -> Self {
        Self {
            model: Mutex::new(None),
            state: AtomicU8::new(0),
        }
    }
}

impl<T> ModelCache<T> {
    fn readiness(&self) -> ModelReadiness {
        match self.state.load(Ordering::Acquire) {
            1 => ModelReadiness::Loading,
            2 => ModelReadiness::Ready,
            3 => ModelReadiness::Failed,
            _ => ModelReadiness::Unloaded,
        }
    }

    fn get_or_load(&self, load: impl FnOnce() -> Result<T, String>) -> Result<Arc<T>, String> {
        let mut cache = self.model.lock().unwrap();
        if let Some(model) = cache.as_ref() {
            return Ok(model.clone());
        }
        self.state.store(1, Ordering::Release);
        match load() {
            Ok(model) => {
                let model = Arc::new(model);
                *cache = Some(model.clone());
                self.state.store(2, Ordering::Release);
                Ok(model)
            }
            Err(error) => {
                self.state.store(3, Ordering::Release);
                Err(error)
            }
        }
    }

    fn clear(&self) {
        let mut cache = self.model.lock().unwrap();
        *cache = None;
        self.state.store(0, Ordering::Release);
    }
}

pub fn model_readiness() -> ModelReadiness {
    CACHED_MODEL.readiness()
}
pub fn playback_busy() -> bool {
    ACTIVE_SINK
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|player| !player.empty())
}

pub(crate) fn phrase_fingerprint(voice: &Path) -> Result<String, String> {
    let mut digest = Sha256::new();
    digest.update(format!(
        "{PUBLIC_REVISION}/{CLONE_REVISION}/pocket-tts-0.6.2/en/phrases-v1"
    ));
    if voice.as_os_str().is_empty() {
        digest.update(DEFAULT_VOICE);
    } else {
        if std::fs::metadata(voice)
            .map_err(|_| "Selected phrase voice is unavailable.")?
            .len()
            > 20_000_000
        {
            return Err("Voice is too large for the phrase cache.".into());
        }
        digest
            .update(std::fs::read(voice).map_err(|_| "Could not read the selected phrase voice.")?);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub(crate) fn render_cached_phrase(
    text: &str,
    voice: &Path,
    path: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<bool, String> {
    let Ok(_inference) = TTS_INFERENCE.try_lock() else {
        return Ok(false);
    };
    if cancelled() || playback_busy() {
        return Ok(false);
    }
    let model = cached_model()?;
    let state = load_voice_state(&model, voice)?;
    let pending = path.with_extension("wav.pending");
    let result = (|| {
        let mut writer = hound::WavWriter::create(
            &pending,
            hound::WavSpec {
                channels: 1,
                sample_rate: model.sample_rate as u32,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )
        .map_err(|_| "Could not create a cached voice phrase.")?;
        let mut count = 0;
        for chunk in model.generate_stream(text, &state) {
            if cancelled() || playback_busy() {
                return Ok(false);
            }
            let samples = chunk
                .and_then(|audio| Ok(audio.flatten_all()?.to_vec1::<f32>()?))
                .map_err(|_| "Could not generate the cached voice phrase.")?;
            count += samples.len();
            if count > model.sample_rate * 30 {
                return Err("Cached phrase exceeded its audio budget.".into());
            }
            for sample in samples {
                writer
                    .write_sample(sample)
                    .map_err(|_| "Could not save cached phrase audio.")?;
            }
        }
        writer
            .finalize()
            .map_err(|_| "Could not finish cached phrase audio.")?;
        if cancelled() {
            return Ok(false);
        }
        std::fs::rename(&pending, path).map_err(|_| "Could not publish cached phrase audio.")?;
        Ok(true)
    })();
    if !matches!(result, Ok(true)) {
        let _ = std::fs::remove_file(pending);
    }
    result
}

pub(crate) fn play_cached_phrase(path: &Path, generation: u64) -> Result<bool, String> {
    let Some(_permit) = SENTENCE_LANE.try_enter() else {
        return Ok(false);
    };
    play_cached_inner(path, generation, || false, None)
}
pub(crate) fn warm_reminder(
    text: &str,
    voice: &Path,
    path: &Path,
    cancel: impl Fn() -> bool,
) -> Result<bool, String> {
    let Some(_permit) = SENTENCE_LANE.try_enter() else {
        return Ok(false);
    };
    render_cached_phrase(text, voice, path, || {
        SENTENCE_LANE.has_waiters() || cancel()
    })
}
pub(crate) fn play_cached_reminder(
    path: &Path,
    text: &str,
    generation: u64,
    cancel: impl Fn() -> bool,
) -> Result<bool, String> {
    let Some(_permit) =
        SENTENCE_LANE.enter(true, || generation != playback_generation() || cancel())
    else {
        return Ok(false);
    };
    play_cached_inner(path, generation, cancel, Some(text))
}
fn play_cached_inner(
    path: &Path,
    generation: u64,
    cancel: impl Fn() -> bool,
    reminder: Option<&str>,
) -> Result<bool, String> {
    if generation != playback_generation() || cancel() || playback_busy() {
        return Ok(false);
    }
    if std::fs::metadata(path)
        .map_err(|_| "Cached phrase is unavailable.")?
        .len()
        > 3_000_000
    {
        return Err("Cached phrase exceeded its audio budget.".into());
    }
    let mut reader = hound::WavReader::open(path).map_err(|_| "Cached phrase audio is invalid.")?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_rate == 0
        || spec.sample_format != hound::SampleFormat::Float
    {
        return Err("Cached phrase format is invalid.".into());
    }
    let samples = reader
        .samples::<f32>()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Cached phrase audio is invalid.")?;
    let output = OUTPUT_DEVICE.lock().unwrap().clone();
    let stream = open_output(output.as_deref())?;
    let sink = Arc::new(rodio::Player::connect_new(stream.mixer()));
    {
        let mut active = ACTIVE_SINK.lock().unwrap();
        if generation != playback_generation()
            || cancel()
            || active.as_ref().is_some_and(|player| !player.empty())
        {
            return Ok(false);
        }
        sink.set_volume(output_volume());
        *active = Some(sink.clone());
    }
    let rate = std::num::NonZeroU32::new(spec.sample_rate).unwrap();
    let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let result = pump_speech_chunks(
        std::iter::once(Ok(samples)),
        spec.sample_rate as usize,
        playback_speed(),
        || generation != playback_generation() || cancel(),
        |samples| {
            let active = ACTIVE_SINK.lock().unwrap();
            if generation == playback_generation()
                && !cancel()
                && active
                    .as_ref()
                    .is_some_and(|player| Arc::ptr_eq(player, &sink))
            {
                use rodio::Source;
                let text = reminder.map(str::to_owned);
                let notified = started.clone();
                sink.append(
                    crate::avatar_bridge::playback_source(samples, rate, generation)
                        .periodic_access(std::time::Duration::from_millis(1), move |_| {
                            if !notified.swap(true, Ordering::SeqCst) {
                                if let Some(text) = &text {
                                    crate::reminders::speech_started(text, generation);
                                }
                            }
                        }),
                );
            }
        },
    );
    while !sink.empty() && generation == playback_generation() && !cancel() {
        if !ACTIVE_SINK
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|player| Arc::ptr_eq(player, &sink))
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    sink.stop();
    let mut active = ACTIVE_SINK.lock().unwrap();
    if active
        .as_ref()
        .is_some_and(|player| Arc::ptr_eq(player, &sink))
    {
        active.take();
    }
    result.map(|_| true)
}

pub fn stop_playback() {
    let next = PLAYBACK_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    crate::avatar_bridge::cancel(next);
    if let Some(sink) = ACTIVE_SINK.lock().unwrap().take() {
        sink.stop();
    }
}

pub fn playback_generation() -> u64 {
    PLAYBACK_GENERATION.load(Ordering::SeqCst)
}

pub fn set_playback_speed(speed: f32) {
    PLAYBACK_SPEED_BITS.store(speed.clamp(0.75, 2.0).to_bits(), Ordering::SeqCst);
}

pub fn normalize_output_volume(volume: f32) -> f32 {
    if volume.is_finite() {
        volume.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

pub fn set_output_volume(volume: f32) {
    let volume = normalize_output_volume(volume);
    OUTPUT_VOLUME_BITS.store(volume.to_bits(), Ordering::SeqCst);
    if let Some(player) = ACTIVE_SINK.lock().unwrap().as_ref() {
        player.set_volume(volume);
    }
}

fn output_volume() -> f32 {
    f32::from_bits(OUTPUT_VOLUME_BITS.load(Ordering::SeqCst))
}

fn playback_speed() -> f32 {
    f32::from_bits(PLAYBACK_SPEED_BITS.load(Ordering::SeqCst))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputDeviceSnapshot {
    pub devices: Vec<String>,
    pub default_device: Option<String>,
}

pub fn output_device_snapshot() -> OutputDeviceSnapshot {
    let host = rodio::cpal::default_host();
    let mut devices = host
        .output_devices()
        .into_iter()
        .flatten()
        .filter_map(|device| device.description().ok().map(|d| d.name().to_string()))
        .filter(|name| !name.trim().is_empty())
        .collect::<Vec<_>>();
    let default_device = host
        .default_output_device()
        .and_then(|device| device.description().ok().map(|d| d.name().to_string()));
    if let Some(name) = &default_device {
        devices.push(name.clone());
    }
    devices.sort();
    devices.dedup();
    OutputDeviceSnapshot {
        devices,
        default_device,
    }
}

pub fn set_output_device(selected: &str, follow_default: bool) {
    *OUTPUT_DEVICE.lock().unwrap() = (!follow_default).then(|| selected.to_string());
}

fn output_device(preferred: Option<&str>) -> Result<rodio::cpal::Device, String> {
    let host = rodio::cpal::default_host();
    match preferred {
        None => host.default_output_device().ok_or_else(|| "No Windows default speakers are available.".into()),
        Some(name) => host.output_devices()
            .map_err(|e| format!("Could not list speakers: {e}"))?
            .find(|device| device.description().is_ok_and(|d| d.name() == name))
            .ok_or_else(|| format!("Selected speakers are unavailable: {name}. Reconnect them or choose another output in Settings.")),
    }
}

fn open_output(preferred: Option<&str>) -> Result<rodio::MixerDeviceSink, String> {
    let device = output_device(preferred)?;
    rodio::DeviceSinkBuilder::from_device(device)
        .and_then(|builder| builder.open_sink_or_fallback())
        .map_err(|e| format!("Audio output is unavailable: {e}"))
}

pub fn test_output_device(preferred: Option<&str>, volume: f32) -> Result<(), String> {
    use rodio::Source;
    let stream = open_output(preferred)?;
    let player = rodio::Player::connect_new(stream.mixer());
    player.set_volume(normalize_output_volume(volume));
    player.append(
        rodio::source::SineWave::new(440.0)
            .take_duration(std::time::Duration::from_millis(400))
            .amplify(0.1),
    );
    player.sleep_until_end();
    Ok(())
}

pub async fn install_standard() -> Result<(), String> {
    let client = reqwest::Client::builder()
        .build()
        .map_err(|_| "Could not create model download client.")?;
    let dir = asset_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|_| "Could not create speech model storage.")?;
    let tokenizer=format!("https://huggingface.co/kyutai/pocket-tts-without-voice-cloning/resolve/{PUBLIC_REVISION}/{TOKENIZER_NAME}");
    download_verified(
        &client,
        &tokenizer,
        None,
        &dir.join(TOKENIZER_NAME),
        TOKENIZER_SHA256,
    )
    .await?;
    let weights=format!("https://huggingface.co/kyutai/pocket-tts-without-voice-cloning/resolve/{PUBLIC_REVISION}/{WEIGHTS_NAME}");
    download_verified(
        &client,
        &weights,
        None,
        &dir.join(WEIGHTS_NAME),
        "58aa704a88faad35f22c34ea1cb55c4c5629de8b8e035c6e4936e2673dc07617",
    )
    .await?;
    CACHED_MODEL.clear();
    Ok(())
}

// One pitch-preserving processor per sentence, carried across generated frames.
// Fixed processing blocks make the result independent of the generator's chunk
// sizes. Compensate both halves of latency and flush, preserving sentence endings.
struct StreamingTempo {
    stretch: Option<signalsmith_stretch::Stretch>,
    pending: Vec<f32>,
    block: usize,
    speed: f64,
    input_samples: usize,
    processed_samples: usize,
    generated_samples: usize,
    emitted_samples: usize,
    skip: usize,
}
impl StreamingTempo {
    fn new(sample_rate: usize, speed: f32) -> Self {
        let speed = f64::from(speed.clamp(0.75, 2.));
        let stretch = (speed != 1.)
            .then(|| signalsmith_stretch::Stretch::preset_default(1, sample_rate as u32));
        let skip = stretch.as_ref().map_or(0, |s| {
            (s.input_latency() as f64 / speed).round() as usize + s.output_latency()
        });
        Self {
            stretch,
            pending: Vec::new(),
            block: (sample_rate / 50).max(1),
            speed,
            input_samples: 0,
            processed_samples: 0,
            generated_samples: 0,
            emitted_samples: 0,
            skip,
        }
    }
    fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        self.processed_samples += samples.len();
        // Accumulate fractional samples instead of rounding each frame's ratio.
        let total = (self.processed_samples as f64 / self.speed).round() as usize;
        let mut output = vec![0.; total - self.generated_samples];
        self.generated_samples = total;
        self.stretch.as_mut().unwrap().process(samples, &mut output);
        self.trim_latency(output)
    }
    fn trim_latency(&mut self, mut output: Vec<f32>) -> Vec<f32> {
        let skip = self.skip.min(output.len());
        output.drain(..skip);
        self.skip -= skip;
        output
    }
    fn push(&mut self, samples: Vec<f32>) -> Vec<f32> {
        self.input_samples += samples.len();
        if self.stretch.is_none() {
            return samples;
        }
        self.pending.extend(samples);
        let complete = self.pending.len() / self.block * self.block;
        let frames: Vec<f32> = self.pending.drain(..complete).collect();
        let mut output = Vec::new();
        for block in frames.chunks(self.block) {
            output.extend(self.process(block));
        }
        self.emitted_samples += output.len();
        output
    }
    fn finish(mut self) -> Vec<f32> {
        if self.stretch.is_none() || self.input_samples == 0 {
            return Vec::new();
        }
        let pending = std::mem::take(&mut self.pending);
        let mut output = self.process(&pending);
        // Advance the processing position all the way to the last input sample
        // before flushing the remaining output latency (per Signalsmith's API).
        let padding = vec![0.; self.stretch.as_ref().unwrap().input_latency()];
        output.extend(self.process(&padding));
        let mut tail = vec![0.; self.stretch.as_ref().unwrap().output_latency() + 2];
        self.stretch.as_mut().unwrap().flush(&mut tail);
        output.extend(self.trim_latency(tail));
        let target = (self.input_samples as f64 / self.speed).round() as usize;
        output.truncate(target.saturating_sub(self.emitted_samples));
        output
    }
}

fn pump_speech_chunks(
    mut chunks: impl Iterator<Item = Result<Vec<f32>, String>>,
    sample_rate: usize,
    speed: f32,
    cancelled: impl Fn() -> bool,
    mut append: impl FnMut(Vec<f32>),
) -> Result<bool, String> {
    let mut tempo = StreamingTempo::new(sample_rate, speed);
    // A short cushion absorbs frame jitter at accelerated speeds. Never wait
    // for the sentence to finish: once primed, forward each processed frame.
    // Keep this separate from the stretch processor's own latency compensation.
    let startup_samples = if speed > 1. {
        sample_rate.saturating_mul(120) / 1000
    } else {
        0
    };
    let mut primed = startup_samples == 0;
    let mut prepared = Vec::new();
    while !cancelled() {
        let Some(samples) = chunks.next() else {
            let tail = tempo.finish();
            if !primed {
                prepared.extend(tail);
                if !cancelled() && !prepared.is_empty() {
                    append(prepared);
                }
            } else if !cancelled() && !tail.is_empty() {
                append(tail);
            }
            return Ok(!cancelled());
        };
        if cancelled() {
            return Ok(false);
        }
        let samples = tempo.push(samples?);
        if cancelled() {
            return Ok(false);
        }
        if !primed {
            prepared.extend(samples);
            if prepared.len() >= startup_samples && !cancelled() {
                append(std::mem::take(&mut prepared));
                primed = true;
            }
        } else if !samples.is_empty() {
            append(samples);
        }
    }
    Ok(false)
}

pub fn asset_dir() -> PathBuf {
    crate::settings::settings_path().with_file_name("pocket_tts")
}

/// An empty selection uses the bundled Alba preset; explicit missing samples stay errors.
pub fn voice_selection_valid(path: &Path) -> bool {
    path.as_os_str().is_empty() || path.is_file()
}

pub fn installed() -> bool {
    let dir = asset_dir();
    dir.join(TOKENIZER_NAME).is_file() && dir.join(WEIGHTS_NAME).is_file()
}
pub fn unload() {
    stop_playback();
    CACHED_MODEL.clear();
}

async fn expected_cloning_hash(client: &reqwest::Client, token: &str) -> Result<String, String> {
    let url = format!(
        "https://huggingface.co/api/models/kyutai/pocket-tts/tree/{CLONE_REVISION}?recursive=true&expand=true"
    );
    let response = client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| "Could not connect to Hugging Face.".to_string())?;
    if !response.status().is_success() {
        return Err("The Hugging Face token cannot access the gated Pocket TTS model. Check model access and token permissions.".into());
    }
    let entries: Vec<serde_json::Value> = response
        .json()
        .await
        .map_err(|_| "Hugging Face returned invalid model metadata.".to_string())?;
    cloning_hash_from_metadata(&entries)
}

fn cloning_hash_from_metadata(entries: &[serde_json::Value]) -> Result<String, String> {
    let hash = entries
        .iter()
        .find(|entry| entry["path"].as_str() == Some(WEIGHTS_NAME))
        .and_then(|entry| {
            // The raw Hub tree API uses oid; the SDK exposes the same LFS digest as sha256.
            entry["lfs"]["oid"]
                .as_str()
                .or_else(|| entry["lfs"]["sha256"].as_str())
        })
        .ok_or("Hugging Face did not provide a checksum for the pinned Pocket TTS weights.")?;
    if hash.len() == 64 && hash.bytes().all(|b| b == b'*') {
        return Err("Hugging Face hid the model checksum. Accept the Pocket TTS model terms and ensure your token has read access to this gated model.".into());
    }
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Hugging Face returned an invalid model checksum.".into());
    }
    Ok(hash.to_ascii_lowercase())
}

async fn download_verified(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
    destination: &Path,
    expected_hash: &str,
) -> Result<(), String> {
    if destination.is_file() {
        let mut file = tokio::fs::File::open(destination)
            .await
            .map_err(|e| e.to_string())?;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = file.read(&mut buffer).await.map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        if format!("{:x}", digest.finalize()) == expected_hash {
            return Ok(());
        }
    }
    let mut request = client.get(url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|_| "Could not download a Pocket TTS asset.".to_string())?;
    if !response.status().is_success() || response.url().scheme() != "https" {
        return Err("The Pocket TTS download failed or was redirected outside HTTPS.".into());
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_ASSET_BYTES)
    {
        return Err("Pocket TTS asset is unexpectedly large.".into());
    }
    let temporary = destination.with_extension("download");
    let result = async {
        let mut file = tokio::fs::File::create(&temporary)
            .await
            .map_err(|e| e.to_string())?;
        let mut stream = response.bytes_stream();
        let mut hash = Sha256::new();
        let mut total = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "Pocket TTS download was interrupted.".to_string())?;
            total += chunk.len() as u64;
            if total > MAX_ASSET_BYTES {
                return Err("Pocket TTS asset is unexpectedly large.".into());
            }
            file.write_all(&chunk).await.map_err(|e| e.to_string())?;
            hash.update(&chunk);
        }
        file.sync_all().await.map_err(|e| e.to_string())?;
        drop(file);
        if format!("{:x}", hash.finalize()) != expected_hash {
            return Err("Pocket TTS checksum mismatch.".into());
        }
        if destination.exists() {
            tokio::fs::remove_file(destination)
                .await
                .map_err(|e| e.to_string())?;
        }
        tokio::fs::rename(&temporary, destination)
            .await
            .map_err(|e| e.to_string())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

pub async fn install(token: &str) -> Result<(), String> {
    if token.trim().is_empty() {
        return Err(
            "Enter and save a Hugging Face token in Settings > Text to speech first.".into(),
        );
    }
    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| e.to_string())?;
    let hash = expected_cloning_hash(&client, token).await?;
    let dir = asset_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    let tokenizer_url = format!("https://huggingface.co/kyutai/pocket-tts-without-voice-cloning/resolve/{PUBLIC_REVISION}/{TOKENIZER_NAME}");
    download_verified(
        &client,
        &tokenizer_url,
        None,
        &dir.join(TOKENIZER_NAME),
        TOKENIZER_SHA256,
    )
    .await?;
    let weights_url =
        format!("https://huggingface.co/kyutai/pocket-tts/resolve/{CLONE_REVISION}/{WEIGHTS_NAME}");
    download_verified(
        &client,
        &weights_url,
        Some(token),
        &dir.join(WEIGHTS_NAME),
        &hash,
    )
    .await?;
    CACHED_MODEL.clear();
    Ok(())
}

pub fn speak_if_current(text: &str, voice_path: &Path, generation: u64) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.send(text.to_string());
    drop(tx);
    play_segments(rx, voice_path, generation)
}

pub fn preload() -> Result<(), String> {
    cached_model().map(|_| ())
}

pub(crate) fn cached_model() -> Result<Arc<pocket_tts::TTSModel>, String> {
    CACHED_MODEL.get_or_load(|| {
        let dir = asset_dir();
        let config = include_bytes!("../assets/pocket-tts-b6369a24.yaml");
        let weights = std::fs::read(dir.join(WEIGHTS_NAME))
            .map_err(|_| "Install Pocket TTS before speaking.".to_string())?;
        let tokenizer = std::fs::read(dir.join(TOKENIZER_NAME))
            .map_err(|_| "Install Pocket TTS before speaking.".to_string())?;
        pocket_tts::TTSModel::load_from_bytes(config, &weights, &tokenizer)
            .map_err(|e| format!("Pocket TTS could not load: {e}"))
    })
}

fn load_voice_state(
    model: &pocket_tts::TTSModel,
    voice_path: &Path,
) -> Result<pocket_tts::ModelState, String> {
    if voice_path.as_os_str().is_empty() {
        model.get_voice_state_from_prompt_bytes(DEFAULT_VOICE)
    } else if voice_path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("safetensors"))
    {
        let prompt = crate::voices::read_prompt(voice_path, model.dim)?;
        model.get_voice_state_from_prompt_tensor(&prompt)
    } else {
        model.get_voice_state(voice_path)
    }
    .map_err(|e| format!("Voice could not be loaded: {e}"))
}

pub fn play_segments(
    segments: std::sync::mpsc::Receiver<String>,
    voice_path: &Path,
    generation: u64,
) -> Result<(), String> {
    play_segments_observed(segments, voice_path, generation, Arc::new(|| false), None)
}
pub fn play_segments_observed(
    segments: std::sync::mpsc::Receiver<String>,
    voice_path: &Path,
    generation: u64,
    cancel: Arc<dyn Fn() -> bool + Send + Sync>,
    on_sentence: Option<Arc<dyn Fn(String) + Send + Sync>>,
) -> Result<(), String> {
    play_sentence_iter(
        segments.into_iter(),
        voice_path,
        generation,
        cancel,
        on_sentence,
    )
}

pub(crate) fn play_sentence_iter(
    segments: impl Iterator<Item = String>,
    voice_path: &Path,
    generation: u64,
    cancel: Arc<dyn Fn() -> bool + Send + Sync>,
    on_sentence: Option<Arc<dyn Fn(String) + Send + Sync>>,
) -> Result<(), String> {
    play_sentence_iter_inner(segments, voice_path, generation, cancel, on_sentence, false)
}

pub(crate) fn play_reminder(
    text: &str,
    voice: &Path,
    generation: u64,
    cancel: Arc<dyn Fn() -> bool + Send + Sync>,
) -> Result<(), String> {
    play_sentence_iter_inner(
        std::iter::once(text.to_owned()),
        voice,
        generation,
        cancel,
        None,
        true,
    )
}

fn play_sentence_iter_inner(
    mut segments: impl Iterator<Item = String>,
    voice_path: &Path,
    generation: u64,
    cancel: Arc<dyn Fn() -> bool + Send + Sync>,
    on_sentence: Option<Arc<dyn Fn(String) + Send + Sync>>,
    priority: bool,
) -> Result<(), String> {
    use rodio::Source;
    if generation != playback_generation() || cancel() {
        return Ok(());
    }
    // Do not initialize audio or replace the active sink before a pipeline reaches speech.
    let Some(first) = segments.next() else {
        return Ok(());
    };
    if generation != playback_generation() || cancel() {
        return Ok(());
    }
    if !voice_selection_valid(voice_path) {
        return Err("Selected voice is unavailable.".into());
    }
    let mut first_permit =
        SENTENCE_LANE.enter(priority, || generation != playback_generation() || cancel());
    if first_permit.is_none() {
        return Ok(());
    }
    let model = cached_model()?;
    let voice = load_voice_state(&model, voice_path)?;
    let selected_output = OUTPUT_DEVICE.lock().unwrap().clone();
    let stream = open_output(selected_output.as_deref())?;
    let sink = Arc::new(rodio::Player::connect_new(stream.mixer()));
    let sample_rate = std::num::NonZeroU32::new(model.sample_rate as u32)
        .ok_or("Invalid Pocket TTS sample rate")?;
    let result = (|| -> Result<(), String> {
        for text in std::iter::once(first).chain(segments) {
            let permit = first_permit.take().or_else(|| {
                SENTENCE_LANE.enter(priority, || generation != playback_generation() || cancel())
            });
            let Some(permit) = permit else {
                break;
            };
            if generation != playback_generation() || cancel() {
                break;
            }
            if text.trim().is_empty() {
                continue;
            }
            {
                let mut active = ACTIVE_SINK.lock().unwrap();
                if generation != playback_generation() || cancel() {
                    break;
                }
                sink.set_volume(output_volume());
                if let Some(previous) = active.replace(sink.clone()) {
                    if !Arc::ptr_eq(&previous, &sink) {
                        previous.stop();
                    }
                }
            }
            let sentence_speed = playback_speed();
            let inference = TTS_INFERENCE.lock().unwrap();
            if generation != playback_generation() || cancel() {
                break;
            }
            // These are streaming frames, not whole buffered sentences. Allow
            // inference to run ahead while audio plays, including above 1x.
            let queue_limit = 25;
            let chunks = model.generate_stream(&text, &voice).map(|audio| {
                let audio = audio.map_err(|e| format!("Speech generation failed: {e}"))?;
                audio
                    .flatten_all()
                    .and_then(|tensor| tensor.to_vec1::<f32>())
                    .map_err(|e| format!("Speech audio could not be read: {e}"))
            });
            let notified = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let completed = pump_speech_chunks(
                chunks,
                model.sample_rate,
                sentence_speed,
                || generation != playback_generation() || cancel(),
                |samples| {
                    // Serialize append with stop_playback: Rodio append can
                    // restart a stopped player, so stale frames must never be
                    // appended after another turn has cancelled this stream.
                    let active = ACTIVE_SINK.lock().unwrap();
                    if generation != playback_generation()
                        || cancel()
                        || !active
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(current, &sink))
                    {
                        return;
                    }
                    let notified = notified.clone();
                    let callback = on_sentence.clone();
                    let sentence = text.clone();
                    sink.append(
                        crate::avatar_bridge::playback_source(samples, sample_rate, generation)
                            .periodic_access(std::time::Duration::from_millis(1), move |_source| {
                                if !notified.swap(true, Ordering::SeqCst) {
                                    if priority {
                                        crate::reminders::speech_started(&sentence, generation);
                                    } else {
                                        crate::assistant_presentation::speech_started(
                                            &sentence, generation,
                                        );
                                    }
                                    if let Some(callback) = &callback {
                                        callback(sentence.clone());
                                    }
                                }
                            }),
                    );
                    drop(active);
                    // Keep generated audio a short distance ahead of playback.
                    while sink.len() > queue_limit
                        && generation == playback_generation()
                        && !cancel()
                    {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                },
            )?;
            drop(inference);
            if !completed {
                break;
            }
            while !sink.empty() && generation == playback_generation() && !cancel() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            drop(permit);
        }
        if cancel() || generation != playback_generation() {
            sink.stop();
        }
        while !sink.empty() {
            if cancel() || generation != playback_generation() {
                sink.stop();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Ok(())
    })();
    if result.is_err() {
        sink.stop();
    }
    let mut active = ACTIVE_SINK.lock().unwrap();
    if active
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, &sink))
    {
        active.take();
    }
    result
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    #[test]
    fn model_readiness_is_visible_during_loading_and_resets_on_unload() {
        let cache = Arc::new(super::ModelCache::<u32>::default());
        assert_eq!(cache.readiness(), super::ModelReadiness::Unloaded);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let worker_cache = cache.clone();
        let worker = std::thread::spawn(move || {
            worker_cache.get_or_load(|| {
                started_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                Ok(42)
            })
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        // This must not block on the weights loader's mutex.
        assert_eq!(cache.readiness(), super::ModelReadiness::Loading);
        finish_tx.send(()).unwrap();
        let loaded = worker.join().unwrap().unwrap();
        assert_eq!(cache.readiness(), super::ModelReadiness::Ready);
        let reused = cache
            .get_or_load(|| panic!("cached weights must not reload"))
            .unwrap();
        assert!(Arc::ptr_eq(&loaded, &reused));
        cache.clear();
        assert_eq!(cache.readiness(), super::ModelReadiness::Unloaded);
    }

    #[test]
    fn model_readiness_reports_failure_and_allows_retry() {
        let cache = super::ModelCache::<u32>::default();
        assert_eq!(
            cache
                .get_or_load(|| Err("invalid weights".into()))
                .unwrap_err(),
            "invalid weights"
        );
        assert_eq!(cache.readiness(), super::ModelReadiness::Failed);
        assert_eq!(*cache.get_or_load(|| Ok(7)).unwrap(), 7);
        assert_eq!(cache.readiness(), super::ModelReadiness::Ready);
    }

    #[test]
    #[ignore = "uses an isolated verified public checkpoint; no network or audio playback"]
    fn public_standard_checkpoint_generates_with_bundled_voice() {
        let path = std::env::var("ECHO_POCKET_STANDARD_TEST_WEIGHTS")
            .expect("Set isolated public weights path");
        let weights = std::fs::read(path).unwrap();
        let tokenizer = std::fs::read(super::asset_dir().join(super::TOKENIZER_NAME)).unwrap();
        let model = pocket_tts::TTSModel::load_from_bytes(
            include_bytes!("../assets/pocket-tts-b6369a24.yaml"),
            &weights,
            &tokenizer,
        )
        .unwrap();
        let voice = model
            .get_voice_state_from_prompt_bytes(super::DEFAULT_VOICE)
            .unwrap();
        let started = std::time::Instant::now();
        let mut samples = Vec::new();
        for frame in model.generate_stream("This is a standard voice test. Every word should remain clear when Echo speaks at twice the usual speed.", &voice) {
            let values = frame
                .unwrap()
                .flatten_all()
                .unwrap()
                .to_vec1::<f32>()
                .unwrap();
            assert!(values.iter().all(|v| v.is_finite()));
            samples.extend(values);
        }
        assert!(samples.len() > 1000);
        eprintln!(
            "Public Pocket TTS generation: {} ms for {} ms of audio",
            started.elapsed().as_millis(),
            samples.len() * 1000 / model.sample_rate
        );
        for speed in [1., 1.5, 2.] {
            let mut output = Vec::new();
            super::pump_speech_chunks(
                samples.chunks(1920).map(|s| Ok(s.to_vec())),
                model.sample_rate,
                speed,
                || false,
                |s| output.extend(s),
            )
            .unwrap();
            assert_eq!(
                output.len(),
                (samples.len() as f64 / speed as f64).round() as usize
            );
            assert!(output.iter().all(|s| s.is_finite()));
            if let Some(dir) = std::env::var_os("ECHO_TEMPO_TEST_OUTPUT_DIR") {
                std::fs::create_dir_all(&dir).unwrap();
                let mut wav = hound::WavWriter::create(
                    std::path::PathBuf::from(dir).join(format!("standard-voice-{speed}x.wav")),
                    hound::WavSpec {
                        channels: 1,
                        sample_rate: model.sample_rate as u32,
                        bits_per_sample: 32,
                        sample_format: hound::SampleFormat::Float,
                    },
                )
                .unwrap();
                for sample in output {
                    wav.write_sample(sample).unwrap();
                }
                wav.finalize().unwrap();
            }
        }
    }

    #[test]
    fn audio_chunks_play_before_generation_finishes_and_cancel_before_next_frame() {
        use std::cell::Cell;
        let generated = Cell::new(0);
        let played = Cell::new(0);
        let frames = (0..3).map(|i| {
            if i > 0 {
                assert_eq!(played.get(), i);
            }
            generated.set(generated.get() + 1);
            Ok(vec![i as f32; 1920])
        });
        assert!(super::pump_speech_chunks(
            frames,
            24_000,
            1.,
            || false,
            |_| played.set(played.get() + 1)
        )
        .unwrap());
        assert_eq!(played.get(), 3);
        generated.set(0);
        let cancelled = Cell::new(false);
        let frames = (0..3).map(|_| {
            generated.set(generated.get() + 1);
            Ok(vec![0.; 1920])
        });
        assert!(!super::pump_speech_chunks(
            frames,
            24_000,
            1.,
            || cancelled.get(),
            |_| cancelled.set(true)
        )
        .unwrap());
        assert_eq!(generated.get(), 1);
        let result = super::pump_speech_chunks(
            std::iter::once(Err("generation failed".into())),
            24_000,
            1.,
            || false,
            |_| panic!(),
        );
        assert_eq!(result.unwrap_err(), "generation failed");
    }

    #[test]
    fn streamed_tempo_is_continuous_across_frame_boundaries() {
        let input: Vec<f32> = (0..48_000)
            .map(|i| ((i as f32 * 440. * std::f32::consts::TAU) / 24_000.).sin())
            .collect();
        for speed in [0.75, 1., 1.5, 2.] {
            let collect = |frame_size| {
                let mut output = Vec::new();
                super::pump_speech_chunks(
                    input.chunks(frame_size).map(|s| Ok(s.to_vec())),
                    24_000,
                    speed,
                    || false,
                    |s| output.extend(s),
                )
                .unwrap();
                output
            };
            let whole = collect(input.len());
            assert_eq!(
                whole.len(),
                (input.len() as f64 / speed as f64).round() as usize
            );
            for size in [157, 1920, 4096] {
                assert_eq!(collect(size), whole);
            }
            if speed < 1. {
                assert!(whole.len() > input.len());
            }
            if speed > 1. {
                assert!(whole.len() < input.len());
            }
            if speed == 1. {
                assert_eq!(whole, input);
            }
        }
    }

    #[test]
    fn accelerated_speech_starts_before_sentence_end_and_cancels_buffered_audio() {
        use std::cell::Cell;
        for speed in [1.25, 1.5, 2.] {
            let generated = Cell::new(0);
            let first_append = Cell::new(None);
            let mut output = Vec::new();
            let frames = (0..50).map(|_| {
                generated.set(generated.get() + 1);
                Ok(vec![0.25; 1920])
            });
            assert!(super::pump_speech_chunks(
                frames,
                24_000,
                speed,
                || false,
                |samples| {
                    if first_append.get().is_none() {
                        first_append.set(Some(generated.get()));
                    }
                    output.extend(samples);
                },
            )
            .unwrap());
            assert!(
                first_append.get().unwrap() < 50,
                "Waited for the entire sentence"
            );
            assert_eq!(output.len(), (96_000. / speed).round() as usize);

            generated.set(0);
            let frames = (0..50).map(|_| {
                generated.set(generated.get() + 1);
                Ok(vec![0.25; 1920])
            });
            assert!(!super::pump_speech_chunks(
                frames,
                24_000,
                speed,
                || generated.get() >= 2,
                |_| panic!("Cancelled startup audio played"),
            )
            .unwrap());
            assert_eq!(generated.get(), 2);

            generated.set(0);
            let stopped = Cell::new(false);
            let frames = (0..50).map(|_| {
                assert!(!stopped.get(), "Generated more audio after stopping");
                generated.set(generated.get() + 1);
                Ok(vec![0.25; 1920])
            });
            assert!(!super::pump_speech_chunks(
                frames,
                24_000,
                speed,
                || stopped.get(),
                |_| stopped.set(true),
            )
            .unwrap());
            assert!(generated.get() < 50);
        }
    }

    #[test]
    fn tempo_preserves_pitch_and_the_last_syllable() {
        for speed in [0.75, 1.25, 1.5, 2.] {
            // The final voiced syllable reaches the very end of the input,
            // catching incomplete flushes that used to discard sentence tails.
            let input: Vec<f32> = (0..24_137)
                .map(|i| {
                    let gain = if i >= 20_000 { 0.8 } else { 0.2 };
                    gain * (i as f32 * 220. * std::f32::consts::TAU / 24_000.).sin()
                })
                .collect();
            let mut tempo = super::StreamingTempo::new(24_000, speed);
            let mut output = Vec::new();
            for frame in input.chunks(157) {
                output.extend(tempo.push(frame.to_vec()));
            }
            output.extend(tempo.finish());
            assert_eq!(
                output.len(),
                (input.len() as f64 / speed as f64).round() as usize
            );
            assert!(output.iter().all(|s| s.is_finite()));
            let interior = &output[2_400..output.len() - 2_400];
            let crossings = interior
                .windows(2)
                .filter(|w| w[0] <= 0. && w[1] > 0.)
                .count();
            let frequency = crossings as f64 * 24_000. / interior.len() as f64;
            assert!((frequency - 220.).abs() < 5., "{speed}x pitch: {frequency}");
            let end = &output[output.len() - 600..output.len() - 100];
            let rms = (end.iter().map(|s| s * s).sum::<f32>() / end.len() as f32).sqrt();
            assert!(rms > 0.3, "{speed}x lost ending: RMS {rms}");
        }
    }

    #[test]
    fn tempo_handles_empty_and_sub_frame_sentences() {
        for speed in [0.75, 1., 1.5, 2.] {
            for length in [0, 1, 157, 479, 480, 481, 1921] {
                let mut tempo = super::StreamingTempo::new(24_000, speed);
                let mut output = tempo.push(vec![0.25; length]);
                output.extend(tempo.finish());
                assert_eq!(
                    output.len(),
                    (length as f64 / speed as f64).round() as usize
                );
                assert!(output.iter().all(|s| s.is_finite()));
            }
        }
    }

    #[test]
    #[ignore = "uses installed Pocket TTS weights; synthetic text, no audio device or playback"]
    fn installed_pocket_stream_yields_audio_before_sentence_finishes() {
        let model = super::cached_model().unwrap();
        let voice = super::load_voice_state(&model, std::path::Path::new("")).unwrap();
        let start = std::time::Instant::now();
        let mut chunks = model.generate_stream("Echo can begin speaking this complete sentence while it is still generating the remaining audio for the sentence.", &voice);
        let first = chunks
            .next()
            .unwrap()
            .unwrap()
            .flatten_all()
            .unwrap()
            .to_vec1::<f32>()
            .unwrap();
        let first_time = start.elapsed();
        assert!(!first.is_empty());
        assert!(first.iter().all(|s| s.is_finite()));
        let mut count = 1;
        for chunk in chunks {
            assert!(chunk.unwrap().elem_count() > 0);
            count += 1;
        }
        assert!(count > 1);
        eprintln!(
            "Pocket TTS audio frames={count} first_frame_ms={} total_ms={}",
            first_time.as_millis(),
            start.elapsed().as_millis()
        );
    }

    #[test]
    #[ignore = "benchmarks installed Pocket TTS with synthetic text; no audio output"]
    fn installed_pocket_accelerated_playback_starts_before_generation_finishes() {
        let model = super::cached_model().unwrap();
        let voice = super::load_voice_state(&model, std::path::Path::new("")).unwrap();
        for speed in [1., 1.25, 1.5, 2.] {
            let start = std::time::Instant::now();
            let generated = std::cell::Cell::new(0);
            let input_samples = std::cell::Cell::new(0usize);
            let first_append = std::cell::Cell::new(None);
            let first_frame = std::cell::Cell::new(None);
            let mut output_samples = 0;
            let chunks = model.generate_stream("Echo can begin speaking this complete sentence while it is still generating the remaining audio for the sentence.", &voice).map(|audio| {
                generated.set(generated.get() + 1);
                if first_frame.get().is_none() { first_frame.set(Some(start.elapsed())); }
                let samples = audio.map_err(|error| error.to_string())?.flatten_all().and_then(|audio| audio.to_vec1::<f32>()).map_err(|error| error.to_string())?;
                input_samples.set(input_samples.get() + samples.len());
                Ok(samples)
            });
            assert!(super::pump_speech_chunks(
                chunks,
                model.sample_rate,
                speed,
                || false,
                |samples| {
                    assert!(samples.iter().all(|sample| sample.is_finite()));
                    output_samples += samples.len();
                    if first_append.get().is_none() {
                        first_append.set(Some((start.elapsed(), generated.get())));
                    }
                }
            )
            .unwrap());
            let (first_audio, first_count) = first_append.get().unwrap();
            assert!(
                first_count < generated.get(),
                "Buffered the complete sentence at {speed}x"
            );
            assert_eq!(
                output_samples,
                (input_samples.get() as f64 / f64::from(speed)).round() as usize
            );
            eprintln!("Pocket playback speed={speed} first_frame_ms={} first_playable_ms={} total_ms={} audio_ms={} first_append_frame={first_count} frames={}", first_frame.get().unwrap().as_millis(), first_audio.as_millis(), start.elapsed().as_millis(), input_samples.get() * 1000 / model.sample_rate, generated.get());
        }
    }

    #[test]
    fn output_volume_is_bounded_and_nonfinite_values_use_default() {
        for (input, expected) in [
            (-1.0, 0.0),
            (0.0, 0.0),
            (0.35, 0.35),
            (2.0, 1.0),
            (f32::NAN, 1.0),
            (f32::INFINITY, 1.0),
        ] {
            assert_eq!(super::normalize_output_volume(input), expected);
        }
    }

    #[test]
    fn default_voice_is_pinned_and_empty_selection_is_valid() {
        use sha2::{Digest, Sha256};
        assert_eq!(
            format!("{:x}", Sha256::digest(super::DEFAULT_VOICE)),
            "ad234695323e4030336b6afc8a050c97e3110603e11ecd8226d9562488300a50"
        );
        assert!(super::voice_selection_valid(std::path::Path::new("")));
        assert!(!super::voice_selection_valid(std::path::Path::new(
            "Echo missing voice 77887c70.wav"
        )));
    }

    #[test]
    #[ignore = "generates a short standard-voice sample with installed Pocket TTS; no playback or network"]
    fn standard_voice_generation_smoke() {
        let model = super::cached_model().unwrap();
        let voice = super::load_voice_state(&model, std::path::Path::new("")).unwrap();
        let audio = model.generate("Hello. This is Echo.", &voice).unwrap();
        let samples = audio.flatten_all().unwrap().to_vec1::<f32>().unwrap();
        assert!(samples.len() > model.sample_rate / 10);
        assert!(samples.iter().all(|v| v.is_finite()));
        assert!(samples.iter().any(|v| v.abs() > 0.001));
    }

    #[test]
    fn cloning_checksum_reads_hub_oid_and_sdk_sha256() {
        let digest = "a1".repeat(32);
        for field in ["oid", "sha256"] {
            let mut lfs = serde_json::Map::new();
            lfs.insert(field.into(), digest.to_uppercase().into());
            let entries = vec![
                serde_json::json!({"path": "other.safetensors", "lfs": {"oid": "00".repeat(32)}}),
                serde_json::json!({"path": super::WEIGHTS_NAME, "lfs": lfs}),
            ];
            assert_eq!(super::cloning_hash_from_metadata(&entries).unwrap(), digest);
        }
    }

    #[test]
    fn cloning_checksum_rejects_missing_invalid_and_unrelated_hashes() {
        for entries in [
            vec![],
            vec![serde_json::json!({"path": "other.safetensors", "lfs": {"oid": "aa".repeat(32)}})],
            vec![
                serde_json::json!({"path": super::WEIGHTS_NAME, "oid": "aa".repeat(20), "xetHash": "aa".repeat(32)}),
            ],
            vec![serde_json::json!({"path": super::WEIGHTS_NAME, "lfs": {"oid": "aa".repeat(20)}})],
            vec![serde_json::json!({"path": super::WEIGHTS_NAME, "lfs": {"oid": "gg".repeat(32)}})],
        ] {
            assert!(super::cloning_hash_from_metadata(&entries).is_err());
        }
    }

    #[test]
    fn cloning_checksum_reports_masked_gated_metadata_as_access_problem() {
        let entries = vec![serde_json::json!({
            "path": super::WEIGHTS_NAME,
            "lfs": {"oid": "*".repeat(64)},
        })];
        let error = super::cloning_hash_from_metadata(&entries).unwrap_err();
        assert!(error.contains("Accept the Pocket TTS model terms"));
        assert!(error.contains("read access"));
    }

    #[test]
    fn missing_selected_speakers_do_not_fall_back_to_another_output() {
        let result = super::output_device(Some("Echo missing speakers 77887c70-65b7-443f"));
        assert!(
            matches!(result, Err(message) if message.contains("Selected speakers are unavailable"))
        );
    }

    #[test]
    #[ignore = "opens actual Windows audio outputs without playing samples"]
    fn speaker_output_device_smoke() {
        let snapshot = super::output_device_snapshot();
        let default = snapshot.default_device.expect("No default output device");
        assert!(snapshot.devices.contains(&default));
        drop(super::open_output(Some(&default)).unwrap());
        drop(super::open_output(None).unwrap());
    }

    #[test]
    fn tempo_changes_duration_without_changing_sample_rate() {
        let input: Vec<f32> = (0..48_000)
            .map(|i| ((i as f32 * 440.0 * std::f32::consts::TAU) / 24_000.0).sin())
            .collect();
        let mut tempo = super::StreamingTempo::new(24_000, 1.5);
        let mut faster = tempo.push(input.clone());
        faster.extend(tempo.finish());
        let mut tempo = super::StreamingTempo::new(24_000, 0.75);
        let mut slower = tempo.push(input.clone());
        slower.extend(tempo.finish());
        assert!(faster.len() < input.len());
        assert!(slower.len() > input.len());
        assert!(faster.iter().all(|sample| sample.is_finite()));
    }
}
