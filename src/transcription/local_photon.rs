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
}

#[derive(Serialize, Deserialize)]
struct RuntimeFile {
    path: String,
    size: u64,
    sha256: String,
}

fn manifest() -> Result<RuntimeManifest, String> {
    let embedded = include_str!(concat!(env!("OUT_DIR"), "/ultra-runtime.json"));
    let local;
    let input = if uses_local_runtime() {
        local = std::fs::read_to_string(local_manifest_path()).map_err(|_| {
            "Build the local Ultra runtime first (see docs/parakeet-ultra.md), then click Install again. No manifest environment variable is required.".to_string()
        })?;
        local.as_str()
    } else {
        embedded
    };
    parse_manifest(input)
}

fn uses_local_runtime() -> bool {
    cfg!(debug_assertions)
        && include_str!(concat!(env!("OUT_DIR"), "/ultra-runtime.json")).trim() == "null"
}

fn local_manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ultra-runtime/runtime-manifest.json")
}

fn local_archive_path(runtime: &RuntimeManifest) -> Result<PathBuf, String> {
    let name = format!("echo-ultra-runtime-{}.zip", &runtime.sha256[..16]);
    Ok(local_manifest_path()
        .parent()
        .ok_or("Missing local runtime directory")?
        .join(name))
}

fn installed_manifest() -> Result<RuntimeManifest, String> {
    if uses_local_runtime() {
        let text = std::fs::read_to_string(runtime_root().join("echo-runtime-receipt.json"))
            .map_err(|_| {
                "Install the local Ultra runtime to create its verification receipt.".to_string()
            })?;
        parse_manifest(&text)
    } else {
        manifest()
    }
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
            .any(|file| safe_relative(&file.path).is_err() || file.sha256.len() != 64)
        || !["python.exe", "helper.py"]
            .iter()
            .all(|required| manifest.files.iter().any(|file| &file.path == required))
    {
        return Err("Invalid pinned GPU runtime manifest".into());
    }
    Ok(manifest)
}

fn root() -> PathBuf {
    dirs_next::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("11th_echo")
        .join("ultra")
}

fn runtime_root() -> PathBuf {
    // Development override; production releases use only their pinned package.
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
    if uses_local_runtime() {
        return match manifest() {
            Ok(_) => "Parakeet Ultra: installs the latest locally built GPU runtime and downloads about 1257 MB of verified model files. Requires an NVIDIA Ampere or newer GPU and a compatible driver.".into(),
            Err(err) => err,
        };
    }
    match manifest() {
        Ok(runtime) => format!("Parakeet Ultra: {:.0} MB of model files and {:.0} MB of GPU runtime. Echo verifies pinned SHA-256 hashes. Requires an NVIDIA Ampere or newer GPU and a compatible driver.", MODEL_FILES.iter().map(|(_, size, _)| size).sum::<u64>() as f64 / 1_000_000.0, runtime.size as f64 / 1_000_000.0),
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

fn extract_runtime(
    archive: &Path,
    destination: &Path,
    runtime: &RuntimeManifest,
) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut seen = std::collections::HashSet::new();
    let declared = runtime
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<std::collections::HashMap<_, _>>();
    if declared.len() != runtime.files.len() {
        return Err("Duplicate GPU runtime manifest path".into());
    }
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
        let relative = safe_relative(entry.name())?;
        if entry.is_dir() || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err("Unexpected directory or link in GPU runtime package".into());
        }
        let file = declared
            .get(entry.name())
            .ok_or("Undeclared GPU runtime file")?;
        if !seen.insert(file.path.to_ascii_lowercase()) || entry.size() != file.size {
            return Err("Invalid GPU runtime file entry".into());
        }
        let target = destination.join(relative);
        std::fs::create_dir_all(target.parent().ok_or("Missing GPU runtime directory")?)
            .map_err(|e| e.to_string())?;
        let mut output = std::fs::File::create(&target).map_err(|e| e.to_string())?;
        let copied = std::io::copy(&mut entry.by_ref().take(file.size + 1), &mut output)
            .map_err(|e| e.to_string())?;
        if copied != file.size {
            return Err("GPU runtime extracted size mismatch".into());
        }
    }
    if seen.len() != runtime.files.len() {
        return Err("GPU runtime package is incomplete".into());
    }
    verify_runtime(destination, runtime)
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

pub async fn download<F>(progress: F) -> Result<(), String>
where
    F: Fn(f32, String) + Send + Sync + 'static,
{
    // Check architecture before fetching gigabytes. CUDA itself is validated by the helper.
    probe_gpu()?;
    let runtime = manifest()?;
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
        let archive = work.join("runtime.zip");
        if uses_local_runtime() {
            (progress)(0.0, "Verifying locally built GPU runtime".into());
            let local_archive = local_archive_path(&runtime)?;
            let destination = archive.clone();
            let size = runtime.size;
            let checksum = runtime.sha256.clone();
            tokio::task::spawn_blocking(move || {
                verify_file(&local_archive, size, &checksum)?;
                std::fs::copy(&local_archive, destination).map_err(|e| e.to_string())?;
                Ok::<_, String>(())
            })
            .await
            .map_err(|e| e.to_string())??;
        } else {
            download_verified(
                DownloadSpec {
                    url: &runtime.url,
                    destination: &archive,
                    expected_hash: &runtime.sha256,
                    expected_size: runtime.size,
                    progress_start: 0.0,
                    progress_span: 0.55,
                    label: "Downloading GPU runtime",
                },
                progress.clone(),
            )
            .await?;
        }
        (progress)(0.55, "Extracting and verifying GPU runtime".into());
        let staging = work.join("runtime");
        let runtime = tokio::task::spawn_blocking(move || {
            extract_runtime(&archive, &staging, &runtime)?;
            Ok::<_, String>(runtime)
        })
        .await
        .map_err(|e| e.to_string())??;
        tokio::fs::write(
            work.join("runtime/echo-runtime-receipt.json"),
            serde_json::to_vec(&runtime).map_err(|e| e.to_string())?,
        )
        .await
        .map_err(|e| e.to_string())?;
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
                "Cannot start the bundled GPU runtime. Download and install to repair.".to_string()
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
    #[ignore = "requires a built optional runtime package; set ECHO_ULTRA_TEST_MANIFEST"]
    fn packaged_runtime_extracts_and_verifies_with_production_rules() {
        let manifest_path = PathBuf::from(
            std::env::var_os("ECHO_ULTRA_TEST_MANIFEST").expect("runtime manifest path"),
        );
        let runtime: RuntimeManifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        let name = reqwest::Url::parse(&runtime.url)
            .unwrap()
            .path_segments()
            .unwrap()
            .next_back()
            .unwrap()
            .to_string();
        let archive = manifest_path.parent().unwrap().join(name);
        verify_file(&archive, runtime.size, &runtime.sha256).unwrap();
        let work = std::env::temp_dir().join(format!(
            "echo-ultra-package-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        extract_runtime(&archive, &work, &runtime).unwrap();
        assert!(work.join("python.exe").is_file());
        assert!(work.join("helper.py").is_file());
        std::fs::remove_dir_all(work).unwrap();
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
    fn local_archive_uses_checksum_name_instead_of_publication_url() {
        let runtime = RuntimeManifest {
            protocol: 1,
            url: "https://updates.example.invalid/ignored.zip".into(),
            size: 1,
            sha256: "a".repeat(64),
            files: vec![],
        };
        assert_eq!(
            local_archive_path(&runtime).unwrap(),
            local_manifest_path()
                .parent()
                .unwrap()
                .join("echo-ultra-runtime-aaaaaaaaaaaaaaaa.zip")
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
            protocol: 1,
            url: "https://localhost/runtime.zip".into(),
            size: 1,
            sha256: "a".repeat(64),
            files: files.into(),
        };
        assert!(parse_manifest(&serde_json::to_string(&runtime).unwrap()).is_ok());
        runtime.sha256 = "x".repeat(64);
        assert!(parse_manifest(&serde_json::to_string(&runtime).unwrap()).is_err());
    }

    #[test]
    fn runtime_extraction_verifies_every_declared_file_and_rolls_back_failed_activation() {
        let work = std::env::temp_dir().join(format!(
            "echo-ultra-extraction-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&work).unwrap();
        let archive = work.join("runtime.zip");
        let bytes = b"runtime fixture";
        let file = RuntimeFile {
            path: "helper.py".into(),
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        };
        let runtime = RuntimeManifest {
            protocol: 1,
            url: "https://example.invalid/runtime.zip".into(),
            size: 1,
            sha256: "0".repeat(64),
            files: vec![file],
        };
        let mut packaged = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        packaged
            .start_file("helper.py", zip::write::SimpleFileOptions::default())
            .unwrap();
        packaged.write_all(bytes).unwrap();
        packaged.finish().unwrap();
        let extracted = work.join("extracted");
        extract_runtime(&archive, &extracted, &runtime).unwrap();
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
}
