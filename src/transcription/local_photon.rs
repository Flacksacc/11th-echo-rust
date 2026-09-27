//! Optional, isolated Photon CUDA runtime. No Python or CUDA dependency is loaded by Echo.
use super::local_sherpa::{download_verified, DownloadSpec};
use super::{AudioChunk, TranscriptionCommand, TranscriptionEvent};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{Receiver, Sender, UnboundedReceiver};

const MAX_FRAME: usize = 256 * 1024;
const REVISION: &str = "73175eb7aeb0d82f1e2a6b53b3aabc10a90bcd0b";
const MODEL_FILES: [(&str, u64, &str); 4] = [
    (
        "config.json",
        1153,
        "e747b85e1bdfd300c8b8ac63bac8dd5221f8fe9bc275b48d06c735fcd6971b6e",
    ),
    (
        "tokenizer.json",
        1159960,
        "bd321b096832a3f270bd3b2a88823957920f1a5c5ada71114a26ea729d0cbe91",
    ),
    (
        "model.safetensors",
        1255353386,
        "c9608f36d0ab956c14bfcc525479b0746b3b42a56f6949ec85c14eb7466717dc",
    ),
    (
        "README.md",
        6433,
        "d4a8b60c83df734ea0373931ba9548f07bdcbab683d19c7f5777a6a83f0971c8",
    ),
];

#[derive(Serialize, Deserialize)]
struct RuntimeManifest {
    protocol: u32,
    url: String,
    size: u64,
    sha256: String,
    files: Vec<RuntimeFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dependency_fingerprint: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct RuntimeFile {
    path: String,
    size: u64,
    sha256: String,
}

fn installed_manifest() -> Result<RuntimeManifest, String> {
    let text = std::fs::read_to_string(runtime_root().join("echo-runtime-receipt.json"))
        .map_err(|_| "Install Ultra to create its verification receipt.".to_string())?;
    // Version 0.1.9 also wrote receipts. Preserve those installations without
    // depending on a source tree or a retired website runtime archive.
    parse_manifest(&text)
}

fn parse_manifest(input: &str) -> Result<RuntimeManifest, String> {
    let manifest: RuntimeManifest = serde_json::from_str(input)
        .map_err(|_| "Parakeet Ultra runtime metadata is missing or invalid.".to_string())?;
    if manifest.protocol != 1
        || manifest.size == 0
        || manifest.sha256.len() != 64
        || !manifest.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !manifest.url.starts_with("https://")
        || manifest.files.is_empty()
        || manifest
            .files
            .iter()
            .any(|file| safe_relative(&file.path).is_err() || !valid_hash(&file.sha256))
        || manifest
            .dependency_fingerprint
            .as_deref()
            .is_some_and(|value| value != dependency_fingerprint())
        || !["python.exe", "helper.py"]
            .iter()
            .all(|required| manifest.files.iter().any(|file| &file.path == required))
    {
        return Err(
            "GPU runtime verification receipt is invalid or outdated. Install Ultra to repair."
                .into(),
        );
    }
    Ok(manifest)
}

const DEPENDENCIES: &str = include_str!("../../runtime/ultra/dependencies.json");

#[derive(Deserialize)]
struct UpstreamDependencies {
    protocol: u32,
    python: UpstreamArtifact,
    pip: UpstreamArtifact,
    packages: Vec<UpstreamArtifact>,
}

#[derive(Deserialize)]
struct UpstreamArtifact {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    filename: String,
    url: String,
    size: u64,
    sha256: String,
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn dependency_fingerprint() -> String {
    format!("{:x}", Sha256::digest(DEPENDENCIES.as_bytes()))
}

fn dependencies() -> Result<UpstreamDependencies, String> {
    parse_dependencies(DEPENDENCIES)
}

fn parse_dependencies(text: &str) -> Result<UpstreamDependencies, String> {
    let pins: UpstreamDependencies = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut names = std::collections::HashSet::new();
    if pins.protocol != 1 || pins.packages.is_empty() {
        return Err("Invalid upstream GPU dependency pins".into());
    }
    for artifact in std::iter::once(&pins.python)
        .chain(std::iter::once(&pins.pip))
        .chain(&pins.packages)
    {
        let url = reqwest::Url::parse(&artifact.url).map_err(|e| e.to_string())?;
        if url.scheme() != "https"
            || !matches!(
                url.host_str(),
                Some("www.python.org" | "files.pythonhosted.org" | "download.pytorch.org")
            )
            || !url.username().is_empty()
            || url.password().is_some()
            || artifact.size == 0
            || !valid_hash(&artifact.sha256)
            || safe_relative(&artifact.filename)?.components().count() != 1
            || !names.insert(artifact.filename.to_ascii_lowercase())
        {
            return Err("Invalid upstream GPU artifact".into());
        }
    }
    if !pins.python.filename.ends_with(".zip")
        || pins.pip.name != "pip"
        || pins
            .packages
            .iter()
            .chain(std::iter::once(&pins.pip))
            .any(|artifact| {
                !artifact.filename.ends_with(".whl")
                    || artifact.name.is_empty()
                    || artifact.version.is_empty()
                    || !artifact
                        .name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    || !artifact
                        .version
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".+_-".contains(&b))
            })
    {
        return Err("Invalid pinned Python wheel".into());
    }
    Ok(pins)
}

fn root() -> PathBuf {
    dirs_next::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("11th_echo")
        .join("ultra")
}

fn runtime_root() -> PathBuf {
    // Development override; ordinary installations use the private verified runtime.
    std::env::var_os("ECHO_ULTRA_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("runtime"))
}

fn model_root() -> PathBuf {
    std::env::var_os("ECHO_ULTRA_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("model"))
}

pub fn available() -> bool {
    runtime_root().join("python.exe").is_file()
        && runtime_root().join("helper.py").is_file()
        && MODEL_FILES.iter().all(|(name, size, _)| {
            std::fs::metadata(model_root().join(name)).is_ok_and(|m| m.len() == *size)
        })
}

pub fn ultra_download_description() -> String {
    match dependencies() {
        Ok(pins) => format!("Parakeet Ultra: {:.0} MB of model files and {:.0} MB of pinned GPU dependencies downloaded directly from Python.org, PyPI, and PyTorch. No Python setup required. Requires an NVIDIA Ampere or newer GPU and a compatible driver. Allow 10 GB of free disk space for installation and repair cache.", MODEL_FILES.iter().map(|(_, size, _)| size).sum::<u64>() as f64 / 1_000_000.0, (pins.python.size + pins.pip.size + pins.packages.iter().map(|p| p.size).sum::<u64>()) as f64 / 1_000_000.0),
        Err(err) => err,
    }
}

pub fn ultra_gpu_status() -> String {
    match probe_gpu() {
        Ok(name) => format!(
            "Detected NVIDIA GPU: {name}. Ultra validates CUDA and available memory when loading."
        ),
        Err(err) => err,
    }
}

fn hidden_command(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}

fn probe_gpu() -> Result<String, String> {
    #[cfg(not(windows))]
    return Err("Parakeet Ultra currently supports Windows x64 only.".into());
    #[cfg(windows)]
    {
        let windows = std::env::var_os("SystemRoot").ok_or("Cannot locate NVIDIA driver tools")?;
        let output = hidden_command(&PathBuf::from(windows).join("System32/nvidia-smi.exe"))
            .args(["--query-gpu=name,compute_cap", "--format=csv,noheader"])
            .output().map_err(|_| "NVIDIA GPU driver not found. Ultra requires Ampere or newer; select V2/V3 for CPU transcription.".to_string())?;
        if output.status.success() {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                if let Some((name, capability)) = line.rsplit_once(',') {
                    if capability.trim().parse::<f32>().is_ok_and(|cap| cap >= 8.0) {
                        return Ok(name.trim().to_string());
                    }
                }
            }
        }
        Err("No compatible NVIDIA GPU detected. Ultra requires Ampere or newer; select V2/V3 for CPU transcription.".into())
    }
}

fn safe_relative(name: &str) -> Result<PathBuf, String> {
    if name.is_empty() || name.contains(['\\', ':']) {
        return Err("Unsafe GPU runtime archive path".into());
    }
    let path = PathBuf::from(name);
    if path.to_string_lossy().replace('\\', "/") != name
        || name.split('/').any(|part| {
            part.is_empty() || part == "." || part == ".." || part.ends_with(['.', ' '])
        })
    {
        return Err("Unsafe GPU runtime archive path".into());
    }
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("Unsafe GPU runtime archive path".into());
    }
    Ok(path)
}

fn verify_file(path: &Path, size: u64, hash: &str) -> Result<(), String> {
    let mut file = std::fs::File::open(path)
        .map_err(|_| "GPU speech files are missing. Download and install to repair.".to_string())?;
    if file.metadata().map_err(|err| err.to_string())?.len() != size {
        return Err(
            "GPU speech file size verification failed. Download and install to repair.".into(),
        );
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|err| err.to_string())?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    if format!("{:x}", hasher.finalize()) != hash {
        return Err(
            "GPU speech file checksum verification failed. Download and install to repair.".into(),
        );
    }
    Ok(())
}

fn verify_runtime(path: &Path, runtime: &RuntimeManifest) -> Result<(), String> {
    for file in &runtime.files {
        verify_file(
            &path.join(safe_relative(&file.path)?),
            file.size,
            &file.sha256,
        )?;
    }
    Ok(())
}

fn activate(staged: &Path, target: &Path) -> Result<(), String> {
    let backup = target.with_extension("old");
    if backup.exists() {
        std::fs::remove_dir_all(&backup).map_err(|e| e.to_string())?;
    }
    let had_target = target.exists();
    if had_target {
        std::fs::rename(target, &backup).map_err(|e| e.to_string())?;
    }
    if let Err(err) = std::fs::rename(staged, target) {
        if had_target {
            let _ = std::fs::rename(&backup, target);
        }
        return Err(err.to_string());
    }
    Ok(())
}

fn extract_python(archive: &Path, target: &Path) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u64;
    std::fs::create_dir_all(target).map_err(|e| e.to_string())?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
        let path = safe_relative(entry.name())?;
        total = total
            .checked_add(entry.size())
            .ok_or("Python archive size overflow")?;
        if path.components().count() != 1
            || entry.is_dir()
            || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
            || total > 100 * 1024 * 1024
            || !seen.insert(entry.name().to_ascii_lowercase())
        {
            return Err("Unexpected embedded Python archive entry".into());
        }
        let mut output = std::fs::File::create(target.join(path)).map_err(|e| e.to_string())?;
        let size = entry.size();
        if std::io::copy(&mut entry.by_ref().take(size + 1), &mut output)
            .map_err(|e| e.to_string())?
            != size
        {
            return Err("Python archive extracted size mismatch".into());
        }
    }
    if !target.join("python.exe").is_file() || !target.join("python312._pth").is_file() {
        return Err("Incomplete embedded Python archive".into());
    }
    // Keep package loading disabled until pip has completed. The final runtime
    // enables only its private lib directory, never system/user site-packages.
    std::fs::write(target.join("python312._pth"), "python312.zip\n.\n").map_err(|e| e.to_string())
}

fn offline_requirements(pins: &UpstreamDependencies) -> String {
    pins.packages
        .iter()
        .map(|p| format!("{}=={} --hash=sha256:{}\n", p.name, p.version, p.sha256))
        .collect()
}

fn install_wheels(
    runtime: &Path,
    cache: &Path,
    work: &Path,
    pins: &UpstreamDependencies,
    progress: &dyn Fn(f32, String),
) -> Result<(), String> {
    let requirements = work.join("requirements.txt");
    std::fs::write(&requirements, offline_requirements(pins)).map_err(|e| e.to_string())?;
    let log_path = work.join("install.log");
    let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let mut child = hidden_command(&runtime.join("python.exe"))
        .args(["-I", "-S", "-c", "import sys,runpy; sys.path.insert(0,sys.argv.pop(1)); runpy.run_module('pip',run_name='__main__')"])
        .arg(cache.join(&pins.pip.filename))
        .args(["--isolated", "--disable-pip-version-check", "--no-input", "--no-cache-dir", "install", "--no-index", "--no-deps", "--only-binary=:all:", "--require-hashes", "--no-compile", "--find-links"])
        .arg(cache)
        .arg("--target").arg(runtime.join("lib"))
        .arg("--requirement").arg(&requirements)
        .env_remove("PYTHONPATH").env_remove("PYTHONHOME")
        .env("PIP_CONFIG_FILE", if cfg!(windows) { "NUL" } else { "/dev/null" })
        .stdin(Stdio::null()).stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
        .stderr(Stdio::from(log))
        .spawn().map_err(|e| format!("Cannot start private GPU dependency installer: {e}"))?;
    #[cfg(windows)]
    let _job = match ProcessJob::attach(&child) {
        Ok(job) => job,
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err);
        }
    };
    let started = Instant::now();
    let mut last_report = Instant::now();
    loop {
        if last_report.elapsed() >= Duration::from_secs(1) {
            progress(
                0.55,
                format!(
                    "Installing pinned GPU dependencies ({}s)",
                    started.elapsed().as_secs()
                ),
            );
            last_report = Instant::now();
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return Err("Pinned GPU dependencies could not be installed. Retry Install Ultra; completed downloads will be reused.".into()),
            Ok(None) if started.elapsed() < Duration::from_secs(900) => std::thread::sleep(Duration::from_millis(100)),
            result => {
                let _ = child.kill(); let _ = child.wait();
                return Err(match result { Err(err) => err.to_string(), _ => "GPU dependency installation timed out. Retry Install Ultra.".into() });
            }
        }
    }
    std::fs::write(
        runtime.join("python312._pth"),
        "python312.zip\n.\nlib\nimport site\n",
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(
        runtime.join("helper.py"),
        include_str!("../../runtime/ultra/helper.py"),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(runtime.join("THIRD_PARTY_NOTICES.txt"), "Echo optional Parakeet Ultra GPU runtime\nDependencies downloaded directly from Python.org, PyPI, and PyTorch; their license files are preserved beneath lib/.\nEmbedded Python license: LICENSE.txt\nParakeet Ultra weights: Moondream / M87 Labs, based on NVIDIA Parakeet V3; CC-BY-4.0.\nhttps://huggingface.co/moondream/parakeet-ultra\nhttps://creativecommons.org/licenses/by/4.0/\n").map_err(|e| e.to_string())?;
    Ok(())
}

fn record_runtime(
    runtime: &Path,
    pins: &UpstreamDependencies,
    progress: &dyn Fn(f32, String),
) -> Result<RuntimeManifest, String> {
    let mut files = Vec::new();
    let mut directories = vec![runtime.to_path_buf()];
    let mut buffer = vec![0; 1024 * 1024];
    let mut last_report = Instant::now();
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                return Err("Link in installed GPU runtime".into());
            }
            if kind.is_dir() {
                if entry.file_name() == "__pycache__" {
                    continue;
                }
                directories.push(entry.path());
                continue;
            }
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "pyc")
                || path
                    .file_name()
                    .is_some_and(|name| name == "echo-runtime-receipt.json")
            {
                continue;
            }
            let relative = path
                .strip_prefix(runtime)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            safe_relative(&relative)?;
            let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            let size = file.metadata().map_err(|e| e.to_string())?.len();
            let mut hash = Sha256::new();
            loop {
                let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
            }
            files.push(RuntimeFile {
                path: relative,
                size,
                sha256: format!("{:x}", hash.finalize()),
            });
            if last_report.elapsed() >= Duration::from_secs(1) {
                progress(
                    0.58,
                    format!("Recording GPU runtime checksums ({} files)", files.len()),
                );
                last_report = Instant::now();
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(RuntimeManifest {
        protocol: 1,
        url: pins.python.url.clone(),
        size: pins.python.size,
        sha256: pins.python.sha256.clone(),
        files,
        dependency_fingerprint: Some(dependency_fingerprint()),
    })
}

async fn prepare_runtime(
    work: &Path,
    cache: &Path,
    pins: UpstreamDependencies,
    progress: Arc<dyn Fn(f32, String) + Send + Sync>,
) -> Result<(), String> {
    tokio::fs::create_dir_all(cache)
        .await
        .map_err(|e| e.to_string())?;
    let artifacts = std::iter::once(&pins.python)
        .chain(std::iter::once(&pins.pip))
        .chain(&pins.packages);
    let total =
        pins.python.size + pins.pip.size + pins.packages.iter().map(|p| p.size).sum::<u64>();
    let mut completed = 0;
    for artifact in artifacts {
        let destination = cache.join(&artifact.filename);
        (progress)(
            0.55 * completed as f32 / total as f32,
            format!("Checking {}", artifact.filename),
        );
        let checked = destination.clone();
        let size = artifact.size;
        let hash = artifact.sha256.clone();
        let cached =
            tokio::task::spawn_blocking(move || verify_file(&checked, size, &hash).is_ok())
                .await
                .map_err(|e| e.to_string())?;
        if !cached {
            // Use a private partial file so retries/other processes cannot race
            // on a shared cache download. Only verified artifacts enter cache.
            let downloaded = work.join(&artifact.filename);
            download_verified(
                DownloadSpec {
                    url: &artifact.url,
                    destination: &downloaded,
                    expected_hash: &artifact.sha256,
                    expected_size: artifact.size,
                    progress_start: 0.55 * completed as f32 / total as f32,
                    progress_span: 0.55 * artifact.size as f32 / total as f32,
                    label: &format!("Downloading {} {}", artifact.name, artifact.version),
                },
                progress.clone(),
            )
            .await?;
            if destination.exists() {
                tokio::fs::remove_file(&destination)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            tokio::fs::rename(downloaded, &destination)
                .await
                .map_err(|e| e.to_string())?;
        }
        completed += artifact.size;
    }
    (progress)(
        0.55,
        "Installing pinned GPU dependencies (no system Python changes)".into(),
    );
    let work = work.to_path_buf();
    let cache = cache.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let staging = work.join("runtime");
        extract_python(&cache.join(&pins.python.filename), &staging)?;
        install_wheels(&staging, &cache, &work, &pins, progress.as_ref())?;
        (progress)(0.58, "Recording GPU runtime verification receipt".into());
        let receipt = record_runtime(&staging, &pins, progress.as_ref())?;
        parse_manifest(&serde_json::to_string(&receipt).map_err(|e| e.to_string())?)?;
        std::fs::write(
            staging.join("echo-runtime-receipt.json"),
            serde_json::to_vec(&receipt).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| e.to_string())?
}

pub async fn download<F>(progress: F) -> Result<(), String>
where
    F: Fn(f32, String) + Send + Sync + 'static,
{
    // Check architecture before fetching gigabytes. CUDA itself is validated by the helper.
    probe_gpu()?;
    let pins = dependencies()?;
    let progress: Arc<dyn Fn(f32, String) + Send + Sync> = Arc::new(progress);
    let base = root();
    tokio::fs::create_dir_all(&base)
        .await
        .map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let work = base.join(format!("download-{stamp}"));
    tokio::fs::create_dir_all(work.join("model"))
        .await
        .map_err(|e| e.to_string())?;
    let result = async {
        let cache = base.join("download-cache").join(dependency_fingerprint());
        prepare_runtime(&work, &cache, pins, progress.clone()).await?;
        let total = MODEL_FILES.iter().map(|(_, size, _)| size).sum::<u64>();
        let mut completed = 0;
        for (name, size, hash) in MODEL_FILES {
            let url = format!(
                "https://huggingface.co/moondream/parakeet-ultra/resolve/{REVISION}/{name}"
            );
            download_verified(
                DownloadSpec {
                    url: &url,
                    destination: &work.join("model").join(name),
                    expected_hash: hash,
                    expected_size: size,
                    progress_start: 0.60 + 0.38 * completed as f32 / total as f32,
                    progress_span: 0.38 * size as f32 / total as f32,
                    label: "Downloading Parakeet Ultra",
                },
                progress.clone(),
            )
            .await?;
            completed += size;
        }
        // Release loaded Python/DLLs before replacing the runtime on Windows.
        super::local_sherpa::unload_local_engine();
        activate(&work.join("runtime"), &base.join("runtime"))?;
        activate(&work.join("model"), &base.join("model"))?;
        (progress)(1.0, "Parakeet Ultra installed".into());
        Ok(())
    }
    .await;
    let _ = tokio::fs::remove_dir_all(work).await;
    result
}

#[derive(Debug, Serialize, Deserialize)]
struct Frame {
    protocol: u32,
    session: u64,
    kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    audio: Vec<i16>,
}

impl Frame {
    fn new(session: u64, kind: &str) -> Self {
        Self {
            protocol: 1,
            session,
            kind: kind.into(),
            text: String::new(),
            audio: Vec::new(),
        }
    }
}

fn write_frame(output: &mut impl Write, frame: &Frame) -> Result<(), String> {
    let bytes = serde_json::to_vec(frame).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FRAME {
        return Err("GPU audio message exceeds the protocol limit".into());
    }
    output
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .and_then(|_| output.write_all(&bytes))
        .and_then(|_| output.flush())
        .map_err(|_| "GPU helper disconnected".into())
}

fn read_frame(input: &mut impl Read) -> Result<Frame, String> {
    let mut header = [0; 4];
    input
        .read_exact(&mut header)
        .map_err(|_| "GPU helper disconnected".to_string())?;
    let size = u32::from_le_bytes(header) as usize;
    if size == 0 || size > MAX_FRAME {
        return Err("Invalid GPU helper message size".into());
    }
    let mut bytes = vec![0; size];
    input
        .read_exact(&mut bytes)
        .map_err(|_| "Incomplete GPU helper message".to_string())?;
    let frame: Frame =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid GPU helper message".to_string())?;
    if frame.protocol != 1 || frame.text.chars().count() > super::MAX_TRANSCRIPT_CHARACTERS {
        return Err("Unsupported GPU helper message".into());
    }
    Ok(frame)
}

struct Helper {
    child: Arc<Mutex<Child>>,
    input: ChildStdin,
    responses: crossbeam_channel::Receiver<Result<Frame, String>>,
    #[cfg(windows)]
    _job: ProcessJob,
}

#[cfg(windows)]
struct ProcessJob(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl ProcessJob {
    fn attach(child: &Child) -> Result<Self, String> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::JobObjects::*;
        let job = Self(
            unsafe { CreateJobObjectW(None, windows::core::PCWSTR::null()) }
                .map_err(|e| e.to_string())?,
        );
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            )
            .and_then(|_| AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle() as isize)))
        }
        .map_err(|_| "Cannot isolate the GPU helper lifetime on Windows".to_string())?;
        Ok(job)
    }
}

#[cfg(windows)]
impl Drop for ProcessJob {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}

pub(super) struct PhotonEngine {
    helper: Mutex<Helper>,
    child: Arc<Mutex<Child>>,
}

struct CancelSession {
    child: Arc<Mutex<Child>>,
    completed: bool,
}
impl Drop for CancelSession {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self.child.lock().unwrap().kill();
        }
    }
}

impl PhotonEngine {
    pub fn shutdown(&self) {
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
    pub fn is_healthy(&self) -> bool {
        self.child
            .lock()
            .unwrap()
            .try_wait()
            .is_ok_and(|status| status.is_none())
    }
    pub fn load() -> Result<Self, String> {
        let started = Instant::now();
        probe_gpu()?;
        let runtime = runtime_root();
        let model = model_root();
        if std::env::var_os("ECHO_ULTRA_RUNTIME_DIR").is_none() {
            crate::echo_info!("local_model", "Ultra verifying GPU runtime files");
            verify_runtime(&runtime, &installed_manifest()?)?;
        }
        crate::echo_info!("local_model", "Ultra verifying model files");
        for (name, size, hash) in MODEL_FILES {
            verify_file(&model.join(name), size, hash)?;
        }
        crate::echo_info!(
            "local_model",
            "Ultra file verification completed in {:.1}s; loading GPU model",
            started.elapsed().as_secs_f32()
        );
        let mut child = hidden_command(&runtime.join("python.exe"))
            .arg("-I")
            // Ship the helper with Echo so source changes apply to existing
            // installed runtimes without replacing their verified dependencies.
            .arg("-c")
            .arg(include_str!("../../runtime/ultra/helper.py"))
            .arg("--model-dir")
            .arg(&model)
            .env("HF_HUB_OFFLINE", "1")
            .env("HF_HUB_DISABLE_TELEMETRY", "1")
            .env_remove("MOONDREAM_API_KEY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| {
                "Cannot start the private GPU runtime. Download and install to repair.".to_string()
            })?;
        #[cfg(windows)]
        let job = match ProcessJob::attach(&child) {
            Ok(job) => job,
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(err);
            }
        };
        let input = child.stdin.take().ok_or("GPU helper input unavailable")?;
        let mut output = child.stdout.take().ok_or("GPU helper output unavailable")?;
        let child = Arc::new(Mutex::new(child));
        let (tx, responses) = crossbeam_channel::bounded(32);
        std::thread::spawn(move || loop {
            let frame = read_frame(&mut output);
            let failed = frame.is_err();
            if tx.send(frame).is_err() || failed {
                break;
            }
        });
        let helper = Helper {
            child,
            input,
            responses,
            #[cfg(windows)]
            _job: job,
        };
        let frame = helper
            .responses
            .recv_timeout(Duration::from_secs(180))
            .map_err(|_| {
                "GPU model loading timed out. Check the NVIDIA driver and available GPU memory."
                    .to_string()
            })??;
        if frame.session != 0 || frame.kind != "ready" {
            return Err(if frame.kind == "error" {
                frame.text
            } else {
                "Invalid GPU helper startup response".into()
            });
        }
        crate::echo_info!(
            "local_model",
            "Ultra GPU model ready after {:.1}s",
            started.elapsed().as_secs_f32()
        );
        Ok(Self {
            child: helper.child.clone(),
            helper: Mutex::new(helper),
        })
    }

    pub async fn run(
        self: &Arc<Self>,
        mut audio_rx: Receiver<AudioChunk>,
        mut commands: UnboundedReceiver<TranscriptionCommand>,
        events: Sender<TranscriptionEvent>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        static NEXT_SESSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let session = NEXT_SESSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let child = self.child.clone();
        let mut cancellation = CancelSession {
            child,
            completed: false,
        };
        let (tx, rx) = crossbeam_channel::bounded::<Frame>(128);
        let engine = self.clone();
        let mut worker = tokio::task::spawn_blocking(move || engine.session(session, rx, events));
        let mut started = false;
        let mut stopped = false;
        let result = 'session: loop {
            tokio::select! {
                biased;
                result = &mut worker => break result.map_err(|e| e.to_string()).and_then(|r| r),
                command = commands.recv(), if !stopped => match command {
                    Some(TranscriptionCommand::Start) => {
                        if !started { tx.try_send(Frame::new(session, "start")).map_err(|_| "GPU helper queue is full")?; started = true; }
                    }
                    Some(TranscriptionCommand::Stop) => {
                        if !started { tx.try_send(Frame::new(session, "start")).map_err(|_| "GPU helper queue is full")?; started = true; }
                        stopped = true;
                    }
                    None => break Err("GPU transcription control channel closed".into()),
                },
                audio = audio_rx.recv(), if started => match audio {
                    Some(audio) => {
                        for samples in audio.chunks(8192) {
                            let mut frame = Frame::new(session, "audio"); frame.audio = samples.to_vec();
                            if tx.try_send(frame).is_err() {
                                break 'session Err("GPU audio queue overflowed. Recording was cancelled; retry with less GPU load.".into());
                            }
                        }
                    }
                    None => {
                        tx.try_send(Frame::new(session, "stop")).map_err(|_| "GPU helper queue is full")?;
                        break worker.await.map_err(|e| e.to_string()).and_then(|r| r);
                    }
                }
            }
        };
        cancellation.completed = result.is_ok();
        result.map_err(Into::into)
    }

    fn session(
        &self,
        session: u64,
        input: crossbeam_channel::Receiver<Frame>,
        events: Sender<TranscriptionEvent>,
    ) -> Result<(), String> {
        let mut helper = self.helper.lock().unwrap();
        let mut finalizing = None;
        loop {
            match input.recv_timeout(Duration::from_millis(10)) {
                Ok(frame) => {
                    if frame.kind == "stop" {
                        finalizing = Some(Instant::now());
                    }
                    write_frame(&mut helper.input, &frame)?;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    return Err("GPU session cancelled".into())
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            }
            while let Ok(response) = helper.responses.try_recv() {
                let frame = response?;
                if frame.session != session {
                    return Err("GPU helper returned a stale session".into());
                }
                match frame.kind.as_str() {
                    "partial" => {
                        self.send_event(&events, TranscriptionEvent::Partial(frame.text))?
                    }
                    "final" if finalizing.is_some() => {
                        self.send_event(&events, TranscriptionEvent::Committed(frame.text))?;
                        return Ok(());
                    }
                    "error" => return Err(frame.text),
                    _ => return Err("Unexpected GPU helper response".into()),
                }
            }
            if finalizing.is_some_and(|start: Instant| start.elapsed() > Duration::from_secs(85)) {
                return Err("GPU finalization timed out".into());
            }
            if helper
                .child
                .lock()
                .unwrap()
                .try_wait()
                .map_err(|e| e.to_string())?
                .is_some()
            {
                return Err("GPU helper exited unexpectedly. Retry to reload the model.".into());
            }
        }
    }

    fn send_event(
        &self,
        events: &Sender<TranscriptionEvent>,
        mut event: TranscriptionEvent,
    ) -> Result<(), String> {
        loop {
            match events.try_send(event) {
                Ok(()) => return Ok(()),
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    return Err("Transcript receiver closed".into())
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(pending)) => event = pending,
            }
            if !self.is_healthy() {
                return Err("GPU session cancelled".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_engine(mode: &str) -> Arc<PhotonEngine> {
        let python = std::env::var_os("ECHO_TEST_PYTHON").unwrap_or_else(|| "python".into());
        let mut child = hidden_command(Path::new(&python))
            .arg("-I")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime/ultra/fake_helper.py"))
            .arg(mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        #[cfg(windows)]
        let job = ProcessJob::attach(&child).unwrap();
        let input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        let child = Arc::new(Mutex::new(child));
        let (tx, responses) = crossbeam_channel::bounded(32);
        std::thread::spawn(move || loop {
            let frame = read_frame(&mut output);
            let failed = frame.is_err();
            if tx.send(frame).is_err() || failed {
                break;
            }
        });
        assert_eq!(
            responses
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap()
                .kind,
            "ready"
        );
        Arc::new(PhotonEngine {
            child: child.clone(),
            helper: Mutex::new(Helper {
                child,
                input,
                responses,
                #[cfg(windows)]
                _job: job,
            }),
        })
    }

    async fn fixture_session(
        engine: Arc<PhotonEngine>,
    ) -> (Result<(), String>, Vec<TranscriptionEvent>) {
        let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(16);
        let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(16);
        command_tx.send(TranscriptionCommand::Start).unwrap();
        audio_tx.send(vec![1, 2]).await.unwrap();
        audio_tx.send(vec![3]).await.unwrap();
        command_tx.send(TranscriptionCommand::Stop).unwrap();
        drop(audio_tx);
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            engine.run(audio_rx, command_rx, event_tx),
        )
        .await
        .expect("helper session must finish promptly")
        .map_err(|err| err.to_string());
        let mut events = Vec::new();
        while let Some(event) = event_rx.recv().await {
            events.push(event);
        }
        (result, events)
    }

    #[tokio::test]
    #[ignore = "requires Python for subprocess fixture"]
    async fn helper_sessions_drain_audio_and_commit_once() {
        let engine = fake_engine("normal");
        for _ in 0..2 {
            let (result, events) = fixture_session(engine.clone()).await;
            result.unwrap();
            let finals = events
                .iter()
                .filter_map(|event| match event {
                    TranscriptionEvent::Committed(text) => Some(text),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(finals, vec!["samples=3;last=3"]);
            assert!(engine.is_healthy());
        }
        engine.shutdown();
        assert!(!engine.is_healthy());
    }

    #[tokio::test]
    #[ignore = "requires Python for subprocess fixture"]
    async fn helper_faults_never_commit_incomplete_text() {
        for mode in ["crash", "malformed", "stale"] {
            let engine = fake_engine(mode);
            let (result, events) = fixture_session(engine.clone()).await;
            assert!(result.is_err(), "{mode}");
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, TranscriptionEvent::Committed(_))),
                "{mode}"
            );
            engine.shutdown();
        }
    }

    #[tokio::test]
    #[ignore = "requires Python for subprocess fixture"]
    async fn cancelling_session_terminates_helper() {
        let engine = fake_engine("normal");
        let (_audio_tx, audio_rx) = tokio::sync::mpsc::channel(16);
        let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(16);
        command_tx.send(TranscriptionCommand::Start).unwrap();
        let running = engine.clone();
        let task = tokio::spawn(async move { running.run(audio_rx, command_rx, event_tx).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(5), async {
            while engine.is_healthy() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancellation must kill the GPU child");
    }

    #[tokio::test]
    #[ignore = "requires installed Ultra runtime, pinned weights, and supported NVIDIA GPU"]
    async fn gpu_runtime_transcribes_fixture_twice_and_releases_process() {
        let engine = Arc::new(PhotonEngine::load().expect("load real GPU runtime"));
        let wave = sherpa_onnx::Wave::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("installer/assets/models/parakeet-tdt-0.6b-v2-int8/test_wavs/0.wav")
                .to_str()
                .unwrap(),
        )
        .unwrap();
        for _ in 0..2 {
            let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(16);
            let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
            let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(32);
            command_tx.send(TranscriptionCommand::Start).unwrap();
            for samples in wave.samples().chunks(8192) {
                audio_tx
                    .send(
                        samples
                            .iter()
                            .map(|sample| (sample * 32767.0) as i16)
                            .collect(),
                    )
                    .await
                    .unwrap();
            }
            command_tx.send(TranscriptionCommand::Stop).unwrap();
            drop(audio_tx);
            tokio::time::timeout(
                Duration::from_secs(90),
                engine.run(audio_rx, command_rx, event_tx),
            )
            .await
            .unwrap()
            .unwrap();
            let mut finals = Vec::new();
            while let Some(event) = event_rx.recv().await {
                if let TranscriptionEvent::Committed(text) = event {
                    finals.push(text);
                }
            }
            assert_eq!(finals.len(), 1);
            assert!(!finals[0].trim().is_empty());
        }
        engine.shutdown();
        assert!(!engine.is_healthy());
    }
    #[test]
    fn protocol_round_trip_and_rejects_oversized_frames() {
        let mut bytes = Vec::new();
        let mut frame = Frame::new(42, "audio");
        frame.audio = vec![-32768, 0, 32767];
        write_frame(&mut bytes, &frame).unwrap();
        let decoded = read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded.session, 42);
        assert_eq!(decoded.audio, frame.audio);
        assert!(read_frame(&mut ((MAX_FRAME + 1) as u32).to_le_bytes().as_slice()).is_err());
    }

    #[test]
    fn archive_paths_cannot_escape_or_use_windows_streams() {
        for name in [
            "../python.exe",
            "/python.exe",
            "C:/python.exe",
            "lib\\x",
            "lib/x:stream",
            "",
        ] {
            assert!(safe_relative(name).is_err(), "{name}");
        }
        assert_eq!(
            safe_relative("lib/torch/x.dll").unwrap(),
            PathBuf::from("lib/torch/x.dll")
        );
    }

    #[test]
    fn runtime_metadata_rejects_non_hex_archive_checksum() {
        let files = ["python.exe", "helper.py"].map(|path| RuntimeFile {
            path: path.into(),
            size: 1,
            sha256: "a".repeat(64),
        });
        let mut runtime = RuntimeManifest {
            dependency_fingerprint: None,
            protocol: 1,
            url: "https://localhost/runtime.zip".into(),
            size: 1,
            sha256: "a".repeat(64),
            files: files.into(),
        };
        assert!(parse_manifest(&serde_json::to_string(&runtime).unwrap()).is_ok());
        runtime.dependency_fingerprint = Some("old-dependency-set".into());
        assert!(parse_manifest(&serde_json::to_string(&runtime).unwrap()).is_err());
        runtime.dependency_fingerprint = Some(dependency_fingerprint());
        assert!(parse_manifest(&serde_json::to_string(&runtime).unwrap()).is_ok());
        runtime.files[0].sha256 = "x".repeat(64);
        assert!(parse_manifest(&serde_json::to_string(&runtime).unwrap()).is_err());
        runtime.files[0].sha256 = "a".repeat(64);
        runtime.sha256 = "x".repeat(64);
        assert!(parse_manifest(&serde_json::to_string(&runtime).unwrap()).is_err());
    }

    #[test]
    fn runtime_receipt_verifies_files_and_activation_preserves_previous_runtime_on_failure() {
        let work = std::env::temp_dir().join(format!(
            "echo-ultra-extraction-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&work).unwrap();
        let bytes = b"runtime fixture";
        let file = RuntimeFile {
            path: "helper.py".into(),
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        };
        let runtime = RuntimeManifest {
            dependency_fingerprint: None,
            protocol: 1,
            url: "https://example.invalid/runtime.zip".into(),
            size: 1,
            sha256: "0".repeat(64),
            files: vec![file],
        };
        let extracted = work.join("extracted");
        std::fs::create_dir(&extracted).unwrap();
        std::fs::write(extracted.join("helper.py"), bytes).unwrap();
        verify_runtime(&extracted, &runtime).unwrap();
        std::fs::write(extracted.join("helper.py"), b"corrupt fixture").unwrap();
        assert!(verify_runtime(&extracted, &runtime).is_err());
        let target = work.join("installed");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("preserved"), b"old runtime").unwrap();
        assert!(activate(&work.join("missing-staging"), &target).is_err());
        assert_eq!(
            std::fs::read(target.join("preserved")).unwrap(),
            b"old runtime"
        );
        std::fs::remove_dir_all(work).unwrap();
    }

    #[test]
    fn runtime_receipt_excludes_mutable_python_cache_and_itself() {
        let work = std::env::temp_dir().join(format!(
            "echo-receipt-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(work.join("lib/__pycache__")).unwrap();
        for name in [
            "python.exe",
            "helper.py",
            "lib/library.py",
            "lib/__pycache__/generated.pyc",
            "echo-runtime-receipt.json",
        ] {
            std::fs::write(work.join(name), b"fixture").unwrap();
        }
        let receipt = record_runtime(&work, &dependencies().unwrap(), &|_, _| {}).unwrap();
        assert_eq!(
            receipt
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec!["helper.py", "lib/library.py", "python.exe"]
        );
        let parsed = parse_manifest(&serde_json::to_string(&receipt).unwrap()).unwrap();
        verify_runtime(&work, &parsed).unwrap();
        std::fs::write(work.join("lib/__pycache__/generated.pyc"), b"new bytecode").unwrap();
        verify_runtime(&work, &parsed).unwrap();
        std::fs::write(work.join("lib/library.py"), b"tampered source").unwrap();
        assert!(verify_runtime(&work, &parsed).is_err());
        std::fs::remove_dir_all(work).unwrap();
    }

    #[test]
    fn upstream_pins_are_complete_and_offline_requirements_have_no_urls_or_unpinned_packages() {
        let pins = dependencies().unwrap();
        assert_eq!(pins.packages.len(), 35);
        let requirements = offline_requirements(&pins);
        assert_eq!(requirements.lines().count(), pins.packages.len());
        assert!(!requirements.contains("https://"));
        for package in &pins.packages {
            assert!(requirements.contains(&format!(
                "{}=={} --hash=sha256:{}",
                package.name, package.version, package.sha256
            )));
        }
        assert!(requirements.contains("torch==2.11.0+cu130"));
        assert!(requirements.contains("moondream==2.4.1"));
        assert!(ultra_download_description().contains("directly from"));
    }

    #[test]
    fn upstream_pins_reject_unsafe_sources_names_and_missing_hashes() {
        let original: serde_json::Value = serde_json::from_str(DEPENDENCIES).unwrap();
        for (field, value) in [
            ("filename", "../pip.whl"),
            ("url", "http://files.pythonhosted.org/pip.whl"),
            ("url", "https://untrusted.example/pip.whl"),
            ("sha256", "invalid"),
            ("name", "pip --extra-index-url evil"),
        ] {
            let mut changed = original.clone();
            changed["pip"][field] = value.into();
            assert!(
                parse_dependencies(&changed.to_string()).is_err(),
                "{field}: {value}"
            );
        }
        let mut duplicate = original.clone();
        duplicate["packages"][0] = duplicate["pip"].clone();
        assert!(parse_dependencies(&duplicate.to_string()).is_err());
    }

    #[test]
    fn python_extraction_rejects_traversal_duplicates_and_missing_interpreter() {
        let work = std::env::temp_dir().join(format!(
            "echo-python-extraction-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&work).unwrap();
        for (index, names) in [
            vec!["python.exe", "python312._pth"],
            vec!["../escape"],
            vec!["lib/nested"],
            vec!["python.exe", "PYTHON.EXE"],
            vec!["python.exe"],
        ]
        .into_iter()
        .enumerate()
        {
            let archive = work.join(format!("{index}.zip"));
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
            for name in names {
                zip.start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                zip.write_all(b"fixture").unwrap();
            }
            zip.finish().unwrap();
            let result = extract_python(&archive, &work.join(index.to_string()));
            assert_eq!(result.is_ok(), index == 0);
        }
        assert!(!work.join("escape").exists());
        std::fs::remove_dir_all(work).unwrap();
    }

    #[tokio::test]
    #[ignore = "downloads 2.2 GB of pinned upstream dependencies and installs private Python"]
    async fn upstream_runtime_installs_and_verifies_without_system_python() {
        let base = PathBuf::from(
            std::env::var_os("ECHO_ULTRA_INSTALL_TEST_WORK")
                .expect("set isolated validation directory"),
        );
        let work = base.join(format!(
            "install-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&work).unwrap();
        let cache = base.join("cache");
        let progress: Arc<dyn Fn(f32, String) + Send + Sync> = Arc::new(|_, label| {
            if !label.contains('%') {
                eprintln!("{label}");
            }
        });
        prepare_runtime(&work, &cache, dependencies().unwrap(), progress)
            .await
            .unwrap_or_else(|error| {
                let log = std::fs::read_to_string(work.join("install.log")).unwrap_or_default();
                panic!("{error}\n{log}");
            });
        let runtime = work.join("runtime");
        let receipt = parse_manifest(
            &std::fs::read_to_string(runtime.join("echo-runtime-receipt.json")).unwrap(),
        )
        .unwrap();
        verify_runtime(&runtime, &receipt).unwrap();
        let output = hidden_command(&runtime.join("python.exe"))
            .args(["-I", "-c", "import importlib.metadata as m; import torch,moondream,kestrel; assert m.version('moondream')=='2.4.1'; assert m.version('kestrel')=='0.8.1'; assert torch.__version__=='2.11.0+cu130'; print('Pinned upstream runtime imports successfully')"])
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        eprintln!("Verified upstream runtime: {}", runtime.display());
    }
}
