use super::{AudioChunk, TranscriptionCommand, TranscriptionEvent};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig, SileroVadModelConfig,
    VadModelConfig, VoiceActivityDetector,
};
use std::collections::VecDeque;
use std::error::Error;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::{Receiver, Sender, UnboundedReceiver, UnboundedSender};

const SAMPLE_RATE: usize = 16_000;
const FULL_SESSION_LIMIT_SAMPLES: usize = SAMPLE_RATE * 60 * 3;
const MODEL_FOLDER: &str = "parakeet-tdt-0.6b-v2-int8";
const MODEL_ARCHIVE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8.tar.bz2";
const MODEL_ARCHIVE_SHA256: &str =
    "157c157bc51155e03e37d2466522a3a737dd9c72bb25f36eb18912964161e1ad";
const MODEL_ARCHIVE_SIZE: u64 = 482_468_385;
const SILERO_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
const MAX_REDIRECTS: usize = 10;
const MODEL_FILES: [(&str, u64, &str); 5] = [
    (
        "parakeet-tdt-0.6b-v2-int8/encoder.int8.onnx",
        652_184_296,
        "a32b12d17bbbc309d0686fbbcc2987b5e9b8333a7da83fa6b089f0a2acd651ab",
    ),
    (
        "parakeet-tdt-0.6b-v2-int8/decoder.int8.onnx",
        7_257_753,
        "b6bb64963457237b900e496ee9994b59294526439fbcc1fecf705b31a15c6b4e",
    ),
    (
        "parakeet-tdt-0.6b-v2-int8/joiner.int8.onnx",
        1_739_080,
        "7946164367946e7f9f29a122407c3252b680dbae9a51343eb2488d057c3c43d2",
    ),
    (
        "parakeet-tdt-0.6b-v2-int8/tokens.txt",
        9_384,
        "ec182b70dd42113aff6c5372c75cac58c952443eb22322f57bbd7f53977d497d",
    ),
    (
        "silero-vad/silero_vad.onnx",
        643_854,
        "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6",
    ),
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalSherpaConfig {
    pub num_threads: i32,
    pub vad_threshold: f32,
    pub silence_ms: u32,
    pub pre_roll_ms: u32,
    pub post_roll_ms: u32,
    pub min_speech_ms: u32,
    pub max_segment_seconds: u32,
    pub partial_interval_ms: u32,
    pub redecode_full_session: bool,
}

impl Default for LocalSherpaConfig {
    fn default() -> Self {
        Self {
            num_threads: automatic_thread_count(),
            vad_threshold: 0.5,
            silence_ms: 600,
            pre_roll_ms: 250,
            post_roll_ms: 150,
            min_speech_ms: 200,
            max_segment_seconds: 30,
            partial_interval_ms: 1000,
            redecode_full_session: false,
        }
    }
}

impl LocalSherpaConfig {
    pub fn normalized(mut self) -> Self {
        self.num_threads = self.num_threads.clamp(1, physical_core_count());
        self.vad_threshold = self.vad_threshold.clamp(0.1, 0.9);
        self.silence_ms = self.silence_ms.clamp(100, 3000);
        self.pre_roll_ms = self.pre_roll_ms.min(1000);
        self.post_roll_ms = self.post_roll_ms.min(1000);
        self.min_speech_ms = self.min_speech_ms.clamp(50, 2000);
        self.max_segment_seconds = self.max_segment_seconds.clamp(5, 120);
        self.partial_interval_ms = self.partial_interval_ms.clamp(250, 5000);
        self
    }
}

pub fn physical_core_count() -> i32 {
    num_cpus::get_physical().max(1).min(i32::MAX as usize) as i32
}

pub fn automatic_thread_count() -> i32 {
    (physical_core_count() / 2).clamp(1, 4)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalEngineStatus {
    Unloaded,
    Loading,
    Ready,
    Failed(String),
}

struct EngineBundle {
    recognizer: Mutex<OfflineRecognizer>,
    vad_model: PathBuf,
}

enum EngineState {
    Unloaded,
    Loading(i32),
    Ready(i32, Arc<EngineBundle>),
    Failed(String),
}

struct EngineManager {
    state: Mutex<EngineState>,
    changed: Condvar,
}

fn manager() -> &'static Arc<EngineManager> {
    static MANAGER: OnceLock<Arc<EngineManager>> = OnceLock::new();
    MANAGER.get_or_init(|| {
        Arc::new(EngineManager {
            state: Mutex::new(EngineState::Unloaded),
            changed: Condvar::new(),
        })
    })
}

pub fn local_engine_status() -> LocalEngineStatus {
    match &*manager().state.lock().unwrap() {
        EngineState::Unloaded => LocalEngineStatus::Unloaded,
        EngineState::Loading(_) => LocalEngineStatus::Loading,
        EngineState::Ready(_, _) => LocalEngineStatus::Ready,
        EngineState::Failed(message) => LocalEngineStatus::Failed(message.clone()),
    }
}

pub fn preload_local_engine(config: &LocalSherpaConfig) {
    let threads = config.clone().normalized().num_threads;
    let manager = manager().clone();
    {
        let mut state = manager.state.lock().unwrap();
        match &*state {
            EngineState::Loading(key) | EngineState::Ready(key, _) if *key == threads => return,
            _ => *state = EngineState::Loading(threads),
        }
    }

    crate::echo_info!("local_model", "Engine preload started threads={}", threads);

    std::thread::spawn(move || {
        let loaded = load_engine(threads);
        match &loaded {
            Ok(_) => crate::echo_info!(
                "local_model",
                "Engine preload completed threads={}",
                threads
            ),
            Err(err) => crate::echo_error!(
                "local_model",
                "Engine preload failed threads={}: {err}",
                threads
            ),
        }
        let mut state = manager.state.lock().unwrap();
        if matches!(&*state, EngineState::Loading(key) if *key == threads) {
            *state = match loaded {
                Ok(engine) => EngineState::Ready(threads, Arc::new(engine)),
                Err(err) => EngineState::Failed(err),
            };
            manager.changed.notify_all();
        }
    });
}

pub fn wait_for_local_engine() -> Result<(), String> {
    engine().map(|_| ())
}

fn engine() -> Result<Arc<EngineBundle>, String> {
    let manager = manager();
    let mut state = manager.state.lock().unwrap();
    loop {
        match &*state {
            EngineState::Ready(_, engine) => return Ok(engine.clone()),
            EngineState::Failed(message) => return Err(message.clone()),
            EngineState::Unloaded => return Err("Local speech model has not been loaded".into()),
            EngineState::Loading(_) => state = manager.changed.wait(state).unwrap(),
        }
    }
}

fn model_root() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("ELEVENTH_ECHO_MODEL_DIR") {
        return Ok(PathBuf::from(path));
    }
    let downloaded = downloaded_model_root();
    if required_models_exist(&downloaded) {
        return Ok(downloaded);
    }
    let exe = std::env::current_exe().map_err(|err| err.to_string())?;
    Ok(exe
        .parent()
        .ok_or_else(|| "Cannot resolve application directory".to_string())?
        .join("models"))
}

fn downloaded_model_root() -> PathBuf {
    dirs_next::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("11th_echo")
        .join("models")
}

fn required_models_exist(root: &Path) -> bool {
    MODEL_FILES.iter().all(|(relative, size, _)| {
        fs::metadata(root.join(relative)).is_ok_and(|m| m.len() == *size)
    })
}

pub fn local_models_available() -> bool {
    if let Some(path) = std::env::var_os("ELEVENTH_ECHO_MODEL_DIR") {
        return required_models_exist(&PathBuf::from(path));
    }
    if required_models_exist(&downloaded_model_root()) {
        return true;
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.join("models")))
        .is_some_and(|root| required_models_exist(&root))
}

pub async fn download_local_models<F>(progress: F) -> Result<(), String>
where
    F: Fn(f32, String) + Send + Sync + 'static,
{
    crate::echo_info!("local_model", "Runtime model download started");
    let progress = Arc::new(progress);
    let base = downloaded_model_root()
        .parent()
        .ok_or_else(|| "Cannot resolve local model directory".to_string())?
        .to_path_buf();
    tokio::fs::create_dir_all(&base)
        .await
        .map_err(|err| err.to_string())?;
    cleanup_stale_downloads(&base).await;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|err| err.to_string())?
        .as_nanos();
    let work = base.join(format!("model-download-{stamp}"));
    tokio::fs::create_dir_all(&work)
        .await
        .map_err(|err| err.to_string())?;

    let result = async {
        let archive = work.join("parakeet.tar.bz2");
        download_verified(
            DownloadSpec {
                url: MODEL_ARCHIVE_URL,
                destination: &archive,
                expected_hash: MODEL_ARCHIVE_SHA256,
                expected_size: MODEL_ARCHIVE_SIZE,
                progress_start: 0.0,
                progress_span: 0.78,
                label: "Downloading Parakeet model",
            },
            progress.clone(),
        )
        .await?;

        let staging_models = work.join("models");
        let extract_progress = progress.clone();
        let archive_for_extract = archive.clone();
        let staging_for_extract = staging_models.clone();
        tokio::task::spawn_blocking(move || {
            extract_parakeet(&archive_for_extract, &staging_for_extract, extract_progress)
        })
        .await
        .map_err(|err| err.to_string())??;

        let vad_dir = staging_models.join("silero-vad");
        tokio::fs::create_dir_all(&vad_dir)
            .await
            .map_err(|err| err.to_string())?;
        let vad_destination = vad_dir.join("silero_vad.onnx");
        download_verified(
            DownloadSpec {
                url: SILERO_URL,
                destination: &vad_destination,
                expected_hash: MODEL_FILES[4].2,
                expected_size: MODEL_FILES[4].1,
                progress_start: 0.94,
                progress_span: 0.04,
                label: "Downloading voice detector",
            },
            progress.clone(),
        )
        .await?;

        (progress)(0.98, "Verifying local speech files".into());
        verify_hashes_uncached(&staging_models)?;
        install_downloaded_models(&staging_models)?;
        (progress)(1.0, "Local speech model installed".into());
        Ok(())
    }
    .await;

    let _ = tokio::fs::remove_dir_all(&work).await;
    match &result {
        Ok(()) => crate::echo_info!("local_model", "Runtime model download completed"),
        Err(err) => crate::echo_error!("local_model", "Runtime model download failed: {err}"),
    }
    result
}

async fn cleanup_stale_downloads(base: &Path) {
    const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
    let Ok(mut entries) = tokio::fs::read_dir(base).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("model-download-") {
            continue;
        }
        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
        let stale = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= STALE_AFTER);
        if metadata.is_dir() && stale {
            let _ = tokio::fs::remove_dir_all(entry.path()).await;
        }
    }
}

struct DownloadSpec<'a> {
    url: &'a str,
    destination: &'a Path,
    expected_hash: &'a str,
    expected_size: u64,
    progress_start: f32,
    progress_span: f32,
    label: &'a str,
}

async fn download_verified(
    spec: DownloadSpec<'_>,
    progress: Arc<dyn Fn(f32, String) + Send + Sync>,
) -> Result<(), String> {
    crate::echo_info!(
        "local_model",
        "Download started label={} expected_bytes={} url={}",
        spec.label,
        spec.expected_size,
        spec.url
    );
    let parsed_url =
        reqwest::Url::parse(spec.url).map_err(|err| format!("Invalid download URL: {err}"))?;
    if parsed_url.scheme() != "https" {
        return Err(format!("Refusing non-HTTPS model download: {}", spec.url));
    }

    let partial = spec.destination.with_extension("part");
    let result = download_verified_inner(parsed_url, &partial, &spec, progress).await;

    match result {
        Ok(()) => {
            if let Err(err) = tokio::fs::rename(&partial, spec.destination).await {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(format!("Could not stage downloaded model: {err}"));
            }
            crate::echo_info!(
                "local_model",
                "Download verified label={} bytes={}",
                spec.label,
                spec.expected_size
            );
            Ok(())
        }
        Err(err) => {
            let _ = tokio::fs::remove_file(&partial).await;
            Err(err)
        }
    }
}

async fn download_verified_inner(
    url: reqwest::Url,
    partial: &Path,
    spec: &DownloadSpec<'_>,
    progress: Arc<dyn Fn(f32, String) + Send + Sync>,
) -> Result<(), String> {
    let redirect_policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("too many model download redirects");
        }
        if attempt.url().scheme() != "https" {
            return attempt.error("model download redirect was not HTTPS");
        }
        attempt.follow()
    });
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(DOWNLOAD_TIMEOUT)
        .redirect(redirect_policy)
        .build()
        .map_err(|err| format!("Could not configure model download: {err}"))?;
    let response = tokio::time::timeout(RESPONSE_TIMEOUT, client.get(url.clone()).send())
        .await
        .map_err(|_| format!("Timed out waiting for model download response from {url}"))?
        .map_err(|err| format!("Model download request failed: {err}"))?
        .error_for_status()
        .map_err(|err| format!("Model download server returned an error: {err}"))?;
    validate_content_length(response.content_length(), spec.expected_size)?;
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::File::create(partial)
        .await
        .map_err(|err| format!("Could not create model download file: {err}"))?;
    let mut hasher = Sha256::new();
    let mut downloaded = DownloadSize::new(spec.expected_size);
    let mut last_report = Instant::now() - Duration::from_secs(1);
    loop {
        let next = tokio::time::timeout(READ_TIMEOUT, stream.next())
            .await
            .map_err(|_| {
                format!(
                    "Model download stalled for more than {} seconds",
                    READ_TIMEOUT.as_secs()
                )
            })?;
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|err| format!("Could not read model download: {err}"))?;
        let received = downloaded.add_chunk(chunk.len())?;
        file.write_all(&chunk)
            .await
            .map_err(|err| format!("Could not write model download: {err}"))?;
        hasher.update(&chunk);
        let fraction = received as f32 / spec.expected_size as f32;
        if last_report.elapsed() >= Duration::from_millis(100) || received == spec.expected_size {
            (progress)(
                spec.progress_start + spec.progress_span * fraction.clamp(0.0, 1.0),
                format!("{}: {:.0}%", spec.label, fraction * 100.0),
            );
            last_report = Instant::now();
        }
    }
    downloaded.finish()?;
    file.flush()
        .await
        .map_err(|err| format!("Could not flush model download: {err}"))?;
    let actual = format!("{:x}", hasher.finalize());
    if actual != spec.expected_hash {
        return Err(format!("Downloaded file verification failed for {url}"));
    }
    Ok(())
}

fn validate_content_length(content_length: Option<u64>, expected_size: u64) -> Result<(), String> {
    if let Some(actual) = content_length {
        if actual != expected_size {
            return Err(format!(
                "Model download reported an unexpected size: expected {expected_size} bytes, got {actual}"
            ));
        }
    }
    Ok(())
}

struct DownloadSize {
    expected: u64,
    received: u64,
}

impl DownloadSize {
    fn new(expected: u64) -> Self {
        Self {
            expected,
            received: 0,
        }
    }

    fn add_chunk(&mut self, chunk_size: usize) -> Result<u64, String> {
        self.received = self
            .received
            .checked_add(chunk_size as u64)
            .ok_or_else(|| "Model download size overflowed".to_string())?;
        if self.received > self.expected {
            return Err(format!(
                "Model download exceeded the expected size of {} bytes",
                self.expected
            ));
        }
        Ok(self.received)
    }

    fn finish(self) -> Result<(), String> {
        if self.received != self.expected {
            return Err(format!(
                "Model download ended early: expected {} bytes, got {}",
                self.expected, self.received
            ));
        }
        Ok(())
    }
}

fn extract_parakeet(
    archive_path: &Path,
    staging_models: &Path,
    progress: Arc<dyn Fn(f32, String) + Send + Sync>,
) -> Result<(), String> {
    let source = fs::File::open(archive_path).map_err(|err| err.to_string())?;
    let decoder = bzip2::read::BzDecoder::new(source);
    let mut archive = tar::Archive::new(decoder);
    let destination = staging_models.join(MODEL_FOLDER);
    fs::create_dir_all(&destination).map_err(|err| err.to_string())?;
    let wanted = [
        "encoder.int8.onnx",
        "decoder.int8.onnx",
        "joiner.int8.onnx",
        "tokens.txt",
    ];
    let mut extracted = 0u64;
    let total: u64 = MODEL_FILES[..4].iter().map(|(_, size, _)| *size).sum();
    for entry in archive.entries().map_err(|err| err.to_string())? {
        let mut entry = entry.map_err(|err| err.to_string())?;
        let path = entry.path().map_err(|err| err.to_string())?;
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !wanted.contains(&name) {
            continue;
        }
        let target = destination.join(name);
        let mut output = fs::File::create(target).map_err(|err| err.to_string())?;
        let copied = std::io::copy(&mut entry, &mut output).map_err(|err| err.to_string())?;
        extracted += copied;
        (progress)(
            0.78 + 0.16 * (extracted as f32 / total as f32).clamp(0.0, 1.0),
            format!(
                "Extracting local speech model: {:.0}%",
                extracted as f32 / total as f32 * 100.0
            ),
        );
    }
    Ok(())
}

fn verify_hashes_uncached(root: &Path) -> Result<(), String> {
    for (relative, expected_size, expected_hash) in MODEL_FILES {
        let path = root.join(relative);
        let metadata = fs::metadata(&path).map_err(|err| err.to_string())?;
        if metadata.len() != expected_size {
            return Err(format!(
                "Downloaded model has the wrong size: {}",
                path.display()
            ));
        }
        let mut file = fs::File::open(&path).map_err(|err| err.to_string())?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            let count = file.read(&mut buffer).map_err(|err| err.to_string())?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        if format!("{:x}", hasher.finalize()) != expected_hash {
            return Err(format!(
                "Downloaded model verification failed: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn install_downloaded_models(staging_models: &Path) -> Result<(), String> {
    let destination = downloaded_model_root();
    let backup = destination.with_extension("old");
    if backup.exists() {
        fs::remove_dir_all(&backup).map_err(|err| err.to_string())?;
    }
    if destination.exists() {
        fs::rename(&destination, &backup).map_err(|err| err.to_string())?;
    }
    match fs::rename(staging_models, &destination) {
        Ok(()) => {
            if backup.exists() {
                let _ = fs::remove_dir_all(backup);
            }
            Ok(())
        }
        Err(err) => {
            if backup.exists() {
                let _ = fs::rename(backup, destination);
            }
            Err(err.to_string())
        }
    }
}

fn required_file(path: &Path) -> Result<String, String> {
    if !path.is_file() {
        return Err(format!("Required local speech model file is missing: {}. Use the Local CPU download prompt to repair local speech.", path.display()));
    }
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("Model path is not valid UTF-8: {}", path.display()))
}

fn load_engine(threads: i32) -> Result<EngineBundle, String> {
    let root = model_root()?;
    crate::echo_info!(
        "local_model",
        "Loading Sherpa ONNX engine threads={} model_root={}",
        threads,
        root.display()
    );
    verify_model_package(&root)?;
    let model = root.join(MODEL_FOLDER);
    let encoder = required_file(&model.join("encoder.int8.onnx"))?;
    let decoder = required_file(&model.join("decoder.int8.onnx"))?;
    let joiner = required_file(&model.join("joiner.int8.onnx"))?;
    let tokens = required_file(&model.join("tokens.txt"))?;
    let vad_model = root.join("silero-vad").join("silero_vad.onnx");
    required_file(&vad_model)?;

    let mut config = OfflineRecognizerConfig::default();
    config.model_config.transducer = OfflineTransducerModelConfig {
        encoder: Some(encoder),
        decoder: Some(decoder),
        joiner: Some(joiner),
    };
    config.model_config.tokens = Some(tokens);
    config.model_config.provider = Some("cpu".into());
    config.model_config.model_type = Some("nemo_transducer".into());
    config.model_config.num_threads = threads;
    config.decoding_method = Some("greedy_search".into());

    let recognizer = OfflineRecognizer::create(&config)
        .ok_or_else(|| "Sherpa ONNX could not initialize the Parakeet model".to_string())?;
    Ok(EngineBundle {
        recognizer: Mutex::new(recognizer),
        vad_model,
    })
}

fn verify_model_package(root: &Path) -> Result<(), String> {
    let mut signature = String::from("parakeet-v2-int8+silerovad-v1\n");
    for (relative, expected_size, _) in MODEL_FILES {
        let path = root.join(relative);
        let metadata = fs::metadata(&path).map_err(|_| {
            format!("Required local speech model file is missing: {}. Use the Local CPU download prompt to repair local speech.", path.display())
        })?;
        if metadata.len() != expected_size {
            return Err(format!("Local speech model file has the wrong size: {}. Use the Local CPU download prompt to repair local speech.", path.display()));
        }
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        signature.push_str(&format!("{relative}|{}|{modified}\n", metadata.len()));
    }

    let marker = dirs_next::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("11th_echo")
        .join("model-verification.txt");
    if fs::read_to_string(&marker).ok().as_deref() == Some(signature.as_str()) {
        return Ok(());
    }

    for (relative, _, expected_hash) in MODEL_FILES {
        let path = root.join(relative);
        let mut file = fs::File::open(&path).map_err(|err| err.to_string())?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            let count = file.read(&mut buffer).map_err(|err| err.to_string())?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        let actual = format!("{:x}", hasher.finalize());
        if actual != expected_hash {
            return Err(format!("Local speech model verification failed: {}. Use the Local CPU download prompt to repair local speech.", path.display()));
        }
    }

    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    fs::write(marker, signature).map_err(|err| err.to_string())
}

fn decode(engine: &EngineBundle, samples: &[f32]) -> Result<String, String> {
    if samples.is_empty() {
        return Ok(String::new());
    }
    let recognizer = engine.recognizer.lock().unwrap();
    let stream = recognizer.create_stream();
    stream.accept_waveform(SAMPLE_RATE as i32, samples);
    recognizer.decode(&stream);
    stream
        .get_result()
        .map(|result| result.text.trim().to_string())
        .ok_or_else(|| "Sherpa ONNX returned no recognition result".to_string())
}

pub struct LocalSherpaTranscriber {
    config: LocalSherpaConfig,
}

impl LocalSherpaTranscriber {
    pub fn new(config: LocalSherpaConfig) -> Self {
        Self {
            config: config.normalized(),
        }
    }

    pub async fn run(
        &self,
        mut audio_rx: Receiver<AudioChunk>,
        mut command_rx: UnboundedReceiver<TranscriptionCommand>,
        event_tx: Sender<TranscriptionEvent>,
        log_tx: UnboundedSender<String>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let engine = tokio::task::spawn_blocking(engine).await??;
        let vad_path = engine.vad_model.to_string_lossy().into_owned();
        let vad_config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(vad_path),
                threshold: self.config.vad_threshold,
                min_silence_duration: self.config.silence_ms as f32 / 1000.0,
                min_speech_duration: self.config.min_speech_ms as f32 / 1000.0,
                window_size: 512,
                max_speech_duration: self.config.max_segment_seconds as f32,
            },
            sample_rate: SAMPLE_RATE as i32,
            num_threads: 1,
            provider: Some("cpu".into()),
            debug: false,
            ..Default::default()
        };
        let vad = VoiceActivityDetector::create(&vad_config, 120.0)
            .ok_or("Sherpa ONNX could not initialize Silero VAD")?;

        let _ = log_tx.send(format!(
            "Local CPU engine ready (threads={}, partial={}ms)",
            self.config.num_threads, self.config.partial_interval_ms
        ));
        let mut accepting = false;
        let mut stable = Vec::<String>::new();
        let mut history = VecDeque::<f32>::new();
        let mut history_start = 0usize;
        let mut total_samples = 0usize;
        let mut last_stable_end = 0usize;
        let mut full_audio = Vec::<f32>::new();
        let mut full_audio_overflow = false;
        let mut last_partial = Instant::now();

        let stop_reason = loop {
            tokio::select! {
                biased;
                command = command_rx.recv() => match command {
                    Some(TranscriptionCommand::Start) => accepting = true,
                    Some(TranscriptionCommand::Stop) => break "stop_command",
                    None => break "control_channel_closed",
                },
                audio = audio_rx.recv(), if accepting => {
                    let Some(audio) = audio else { break "audio_channel_closed"; };
                    let samples: Vec<f32> = audio.into_iter().map(|sample| sample as f32 / i16::MAX as f32).collect();
                    if !full_audio_overflow {
                        if full_audio.len() + samples.len() <= FULL_SESSION_LIMIT_SAMPLES {
                            full_audio.extend_from_slice(&samples);
                        } else {
                            full_audio.clear();
                            full_audio_overflow = true;
                        }
                    }
                    history.extend(samples.iter().copied());
                    total_samples += samples.len();
                    vad.accept_waveform(&samples);

                    drain_segments(
                        &vad,
                        SegmentDrain {
                            engine: &engine,
                            config: &self.config,
                            stable: &mut stable,
                            history: &history,
                            history_start,
                            last_stable_end: &mut last_stable_end,
                            event_tx: &event_tx,
                            log_tx: &log_tx,
                        },
                    )
                    .await?;

                    let keep_from = last_stable_end.saturating_sub(self.config.pre_roll_ms as usize * SAMPLE_RATE / 1000);
                    if keep_from > history_start {
                        let remove = (keep_from - history_start).min(history.len());
                        history.drain(..remove);
                        history_start += remove;
                    }

                    if last_partial.elapsed() >= Duration::from_millis(self.config.partial_interval_ms as u64) {
                        let active_start = last_stable_end.saturating_sub(history_start);
                        let active: Vec<f32> = history.iter().skip(active_start).copied().collect();
                        if active.len() >= SAMPLE_RATE / 2 {
                            let partial = decode_async(engine.clone(), active).await?;
                            let display = join_transcript(&stable, &partial);
                            if !display.is_empty() {
                                event_tx.send(TranscriptionEvent::Partial(display)).await?;
                            }
                        }
                        last_partial = Instant::now();
                    }
                    let _ = total_samples;
                }
            }
        };

        let _ = log_tx.send(format!(
            "Local finalization started (reason={stop_reason}, captured_samples={total_samples})"
        ));
        vad.flush();
        drain_segments(
            &vad,
            SegmentDrain {
                engine: &engine,
                config: &self.config,
                stable: &mut stable,
                history: &history,
                history_start,
                last_stable_end: &mut last_stable_end,
                event_tx: &event_tx,
                log_tx: &log_tx,
            },
        )
        .await?;

        let trailing_start = last_stable_end.saturating_sub(history_start);
        if trailing_start < history.len() {
            let trailing: Vec<f32> = history.iter().skip(trailing_start).copied().collect();
            if trailing.len() >= SAMPLE_RATE * self.config.min_speech_ms as usize / 1000 {
                let started = Instant::now();
                let text = decode_async(engine.clone(), trailing).await?;
                if !text.is_empty() {
                    stable.push(text);
                }
                let _ = log_tx.send(format!(
                    "Local trailing decode completed in {} ms",
                    started.elapsed().as_millis()
                ));
            }
        }

        let final_text = if self.config.redecode_full_session && !full_audio_overflow {
            let started = Instant::now();
            let text = decode_async(engine.clone(), full_audio).await?;
            let _ = log_tx.send(format!(
                "Local full-session decode completed in {} ms",
                started.elapsed().as_millis()
            ));
            text
        } else {
            if self.config.redecode_full_session && full_audio_overflow {
                let _ = log_tx.send(
                    "Full-session decode skipped because the 3-minute cap was reached".into(),
                );
            }
            stable.join(" ")
        };
        let _ = log_tx.send(format!(
            "Local final transcript committed (characters={})",
            final_text.chars().count()
        ));
        event_tx
            .send(TranscriptionEvent::Committed(final_text))
            .await?;
        Ok(())
    }
}

async fn decode_async(engine: Arc<EngineBundle>, samples: Vec<f32>) -> Result<String, String> {
    tokio::task::spawn_blocking(move || decode(&engine, &samples))
        .await
        .map_err(|err| err.to_string())?
}

struct SegmentDrain<'a> {
    engine: &'a Arc<EngineBundle>,
    config: &'a LocalSherpaConfig,
    stable: &'a mut Vec<String>,
    history: &'a VecDeque<f32>,
    history_start: usize,
    last_stable_end: &'a mut usize,
    event_tx: &'a Sender<TranscriptionEvent>,
    log_tx: &'a UnboundedSender<String>,
}

async fn drain_segments(
    vad: &VoiceActivityDetector,
    context: SegmentDrain<'_>,
) -> Result<(), String> {
    while let Some(segment) = vad.front() {
        let pre = context.config.pre_roll_ms as usize * SAMPLE_RATE / 1000;
        let post = context.config.post_roll_ms as usize * SAMPLE_RATE / 1000;
        let segment_start = segment.start().max(0) as usize;
        let segment_end = segment_start + segment.n().max(0) as usize;
        let wanted_start = segment_start.saturating_sub(pre).max(context.history_start);
        let wanted_end = (segment_end + post).min(context.history_start + context.history.len());
        let samples: Vec<f32> = if wanted_start < wanted_end {
            context
                .history
                .iter()
                .skip(wanted_start - context.history_start)
                .take(wanted_end - wanted_start)
                .copied()
                .collect()
        } else {
            segment.samples().to_vec()
        };
        let started = Instant::now();
        let text = decode_async(context.engine.clone(), samples).await?;
        if !text.is_empty() {
            context.stable.push(text);
            context
                .event_tx
                .send(TranscriptionEvent::Partial(context.stable.join(" ")))
                .await
                .map_err(|err| err.to_string())?;
        }
        *context.last_stable_end = (*context.last_stable_end).max(segment_end);
        let _ = context.log_tx.send(format!(
            "Local VAD segment {:.2}s decoded in {} ms",
            segment.n() as f32 / SAMPLE_RATE as f32,
            started.elapsed().as_millis()
        ));
        drop(segment);
        vad.pop();
    }
    Ok(())
}

fn join_transcript(stable: &[String], partial: &str) -> String {
    match (stable.is_empty(), partial.trim().is_empty()) {
        (true, true) => String::new(),
        (false, true) => stable.join(" "),
        (true, false) => partial.trim().to_string(),
        (false, false) => format!("{} {}", stable.join(" "), partial.trim()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        automatic_thread_count, decode, join_transcript, load_engine, manager,
        validate_content_length, DownloadSize, EngineState, LocalSherpaConfig,
        LocalSherpaTranscriber,
    };
    use crate::transcription::{TranscriptionCommand, TranscriptionEvent};
    use std::sync::Arc;
    use tokio::sync::mpsc;

    #[test]
    fn defaults_are_safe() {
        let config = LocalSherpaConfig::default();
        assert!((1..=4).contains(&config.num_threads));
        assert_eq!(config.silence_ms, 600);
        assert_eq!(config.partial_interval_ms, 1000);
        assert!(!config.redecode_full_session);
    }

    #[test]
    fn normalization_clamps_user_values() {
        let config = LocalSherpaConfig {
            vad_threshold: 5.0,
            silence_ms: 1,
            partial_interval_ms: 20_000,
            ..Default::default()
        }
        .normalized();
        assert_eq!(config.vad_threshold, 0.9);
        assert_eq!(config.silence_ms, 100);
        assert_eq!(config.partial_interval_ms, 5000);
        assert!(automatic_thread_count() >= 1);
    }

    #[test]
    fn partials_append_to_stable_segments() {
        assert_eq!(join_transcript(&["hello".into()], "world"), "hello world");
    }

    #[test]
    fn content_length_must_match_pinned_size_when_present() {
        assert!(validate_content_length(None, 10).is_ok());
        assert!(validate_content_length(Some(10), 10).is_ok());
        assert!(validate_content_length(Some(9), 10).is_err());
        assert!(validate_content_length(Some(11), 10).is_err());
    }

    #[test]
    fn streamed_download_size_rejects_overrun_and_truncation() {
        let mut exact = DownloadSize::new(10);
        assert_eq!(exact.add_chunk(4).unwrap(), 4);
        assert_eq!(exact.add_chunk(6).unwrap(), 10);
        assert!(exact.finish().is_ok());

        let mut oversized = DownloadSize::new(10);
        assert!(oversized.add_chunk(11).is_err());

        let mut truncated = DownloadSize::new(10);
        assert_eq!(truncated.add_chunk(9).unwrap(), 9);
        assert!(truncated.finish().is_err());
    }

    #[test]
    #[ignore = "requires installer model assets"]
    fn local_model_transcribes_known_wav() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("installer")
            .join("assets")
            .join("models");
        std::env::set_var("ELEVENTH_ECHO_MODEL_DIR", &root);
        let engine = load_engine(2).expect("load local engine");
        let wave_path = root
            .join("parakeet-tdt-0.6b-v2-int8")
            .join("test_wavs")
            .join("0.wav");
        let wave = sherpa_onnx::Wave::read(wave_path.to_str().expect("UTF-8 test path"))
            .expect("read test wave");
        let text = decode(&engine, wave.samples()).expect("decode test wave");
        assert!(!text.trim().is_empty());
    }

    #[tokio::test]
    #[ignore = "requires installer model assets"]
    async fn audio_channel_closure_still_commits_the_local_transcript() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("installer")
            .join("assets")
            .join("models");
        std::env::set_var("ELEVENTH_ECHO_MODEL_DIR", &root);
        let threads = 2;
        let engine = load_engine(threads).expect("load local engine");
        *manager().state.lock().unwrap() = EngineState::Ready(threads, Arc::new(engine));

        let wave_path = root
            .join("parakeet-tdt-0.6b-v2-int8")
            .join("test_wavs")
            .join("0.wav");
        let wave = sherpa_onnx::Wave::read(wave_path.to_str().expect("UTF-8 test path"))
            .expect("read test wave");
        let samples = wave
            .samples()
            .iter()
            .map(|sample| (sample * i16::MAX as f32) as i16)
            .collect::<Vec<_>>();

        let (audio_tx, audio_rx) = mpsc::channel(16);
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (log_tx, _log_rx) = mpsc::unbounded_channel();
        let transcriber = LocalSherpaTranscriber::new(LocalSherpaConfig {
            num_threads: threads,
            ..Default::default()
        });
        let task = tokio::spawn(async move {
            transcriber
                .run(audio_rx, command_rx, event_tx, log_tx)
                .await
        });

        command_tx.send(TranscriptionCommand::Start).unwrap();
        for chunk in samples.chunks(16_000) {
            audio_tx.send(chunk.to_vec()).await.unwrap();
        }
        drop(audio_tx);

        let committed = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while let Some(event) = event_rx.recv().await {
                if let TranscriptionEvent::Committed(text) = event {
                    return text;
                }
            }
            String::new()
        })
        .await
        .expect("local finalization should not hang");
        assert!(!committed.trim().is_empty());
        task.await
            .expect("local task should not panic")
            .expect("local task should finish successfully");
    }

    #[tokio::test]
    #[ignore = "downloads the 461 MB production model package"]
    async fn runtime_download_installs_verified_models() {
        let root = super::downloaded_model_root();
        let existed = root.exists();
        super::download_local_models(|_, _| {})
            .await
            .expect("download and install models");
        assert!(super::required_models_exist(&root));
        if !existed {
            std::fs::remove_dir_all(root).expect("remove downloaded test models");
        }
    }
}
