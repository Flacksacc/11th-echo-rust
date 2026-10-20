//! Bounded, optional local transport. Audio observers never wait on the renderer.
use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    LazyLock, Mutex,
};

pub const PROTOCOL: u32 = 1;
pub const MAX_FRAME: usize = 256 * 1024;
static CONNECTED: AtomicBool = AtomicBool::new(false);
static RENDER_READY: AtomicBool = AtomicBool::new(false);
pub fn renderer_ready() -> bool {
    RENDER_READY.load(Ordering::Relaxed)
}
static AUDIO: LazyLock<(
    crossbeam_channel::Sender<AudioFrame>,
    crossbeam_channel::Receiver<AudioFrame>,
)> = LazyLock::new(|| crossbeam_channel::bounded(12));
static STATUS: LazyLock<Mutex<String>> =
    LazyLock::new(|| Mutex::new("PNG portrait · Unreal runtime optional".into()));
static CURSOR: Mutex<(u64, u64)> = Mutex::new((0, 0));

#[derive(Clone, Serialize)]
pub struct AudioFrame {
    pub generation: u64,
    pub start_sample: u64,
    pub sample_rate: u32,
    pub samples: Vec<f32>,
}

pub fn status() -> String {
    STATUS.lock().unwrap().clone()
}
fn set_status(value: &str) {
    *STATUS.lock().unwrap() = value.into();
}
pub fn companion_exited() {
    CONNECTED.store(false, Ordering::SeqCst);
    RENDER_READY.store(false, Ordering::SeqCst);
    set_status("Unreal avatar stopped · using PNG portrait");
}

fn reserve_audio(generation: u64, rate: u32, count: usize) -> u64 {
    crate::assistant_presentation::update(|s| s.generation = generation);
    let mut cursor = CURSOR.lock().unwrap();
    if cursor.0 != generation {
        *cursor = (generation, 0);
    }
    let start_micros = cursor.1;
    cursor.1 += count as u64 * 1_000_000 / rate as u64;
    start_micros
}

pub fn playback_source(
    samples: Vec<f32>,
    rate: std::num::NonZeroU32,
    generation: u64,
) -> impl rodio::Source<Item = f32> + Send {
    let start = reserve_audio(generation, rate.get(), samples.len());
    paced_source(
        samples,
        rate,
        move |offset, samples| {
            if CONNECTED.load(Ordering::Relaxed)
                && generation == crate::assistant_voice::playback_generation()
            {
                let _ = AUDIO.0.try_send(AudioFrame {
                    generation,
                    start_sample: start * rate.get() as u64 / 1_000_000 + offset as u64,
                    sample_rate: rate.get(),
                    samples,
                });
            }
        },
        move |micros| playback_clock(generation, start + micros),
    )
}

use rodio::Source;
fn paced_source(
    samples: Vec<f32>,
    rate: std::num::NonZeroU32,
    mut send: impl FnMut(usize, Vec<f32>) + Send,
    mut clock: impl FnMut(u64) + Send,
) -> impl rodio::Source<Item = f32> + Send {
    let count = samples.len();
    let buffer =
        rodio::buffer::SamplesBuffer::new(std::num::NonZeroU16::new(1).unwrap(), rate, samples);
    let mut observer = buffer.clone();
    let mut sent = 0;
    buffer
        .track_position()
        .periodic_access(std::time::Duration::from_millis(20), move |source| {
            clock(source.get_pos().as_micros() as u64);
            let target =
                (((source.get_pos().as_micros() + 80_000) * rate.get() as u128 / 1_000_000)
                    as usize)
                    .min(count);
            while sent < target {
                let size = (target - sent).min(2048);
                let span = observer.by_ref().take(size).collect();
                send(sent, span);
                sent += size;
            }
        })
}

static CLOCK: Mutex<(u64, u64)> = Mutex::new((0, 0));
pub fn playback_clock(generation: u64, micros: u64) {
    if generation == crate::assistant_voice::playback_generation() {
        if let Ok(mut clock) = CLOCK.try_lock() {
            *clock = (generation, micros);
        }
    }
}
pub fn cancel(generation: u64) {
    crate::assistant_presentation::update(|s| s.generation = generation);
    *CLOCK.lock().unwrap() = (generation, 0);
    while AUDIO.1.try_recv().is_ok() {}
}

pub fn encode(value: &impl Serialize) -> std::io::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME {
        return Err(std::io::Error::other("Avatar frame exceeds limit"));
    }
    let mut frame = Vec::with_capacity(bytes.len() + 4);
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend(bytes);
    Ok(frame)
}

pub struct Companion {
    stop: std::sync::Arc<AtomicBool>,
    child: std::process::Child,
    server: Option<std::thread::JoinHandle<()>>,
    #[cfg(windows)]
    _tree: crate::agent_tools::ProcessTree,
}
impl Drop for Companion {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        CONNECTED.store(false, Ordering::SeqCst);
        RENDER_READY.store(false, Ordering::SeqCst);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}
impl Companion {
    pub fn running(&mut self) -> bool {
        self.child.try_wait().is_ok_and(|exit| exit.is_none())
    }
}

pub fn validate_config(config: &crate::assistant_presentation::AvatarConfig) -> Result<(), String> {
    use std::path::Path;
    let executable = Path::new(&config.runtime);
    if !executable.is_absolute()
        || !executable.is_file()
        || executable
            .extension()
            .is_none_or(|s| !s.eq_ignore_ascii_case("exe"))
    {
        return Err("Choose an existing absolute Unreal companion .exe path.".into());
    }
    if !config.pack.is_empty()
        && (!Path::new(&config.pack).is_absolute()
            || !Path::new(&config.pack).is_file()
            || Path::new(&config.pack)
                .extension()
                .is_none_or(|s| !s.eq_ignore_ascii_case("pak")))
    {
        return Err("Choose an existing prepared .pak file.".into());
    }
    Ok(())
}

#[cfg(windows)]
pub fn launch(config: &crate::assistant_presentation::AvatarConfig) -> Result<Companion, String> {
    use std::{
        os::windows::process::CommandExt,
        path::Path,
        process::{Command, Stdio},
        sync::Arc,
    };
    validate_config(config)?;
    let executable = Path::new(&config.runtime);
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let pipe = format!(
        r"\\.\pipe\EchoAvatar-{}-{}",
        std::process::id(),
        crate::pipelines::new_id()
    );
    let server_name = pipe.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let server = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        match runtime {
            Ok(runtime) => runtime.block_on(serve(server_name, server_stop, ready_tx)),
            Err(_) => {
                let _ = ready_tx.send(Err("Could not start avatar transport.".to_string()));
            }
        }
    });
    match ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(Ok(())) => {}
        result => {
            stop.store(true, Ordering::SeqCst);
            let _ = server.join();
            return Err(result
                .ok()
                .and_then(Result::err)
                .unwrap_or_else(|| "Avatar transport timed out.".into()));
        }
    }
    // Bypass the bootstrap exe so the renderer has an owned process lifetime.
    let game = executable
        .parent()
        .unwrap()
        .join("EchoAvatar/Binaries/Win64/EchoAvatar.exe");
    let executable = if game.is_file() {
        game.as_path()
    } else {
        executable
    };
    let mut command = Command::new(executable);
    command
        .arg(format!("-EchoPipe={pipe}"))
        .arg("-nosound")
        .arg("-RenderOffscreen")
        .arg("-d3d11")
        .arg("-windowed")
        .arg("-ResX=640")
        .arg("-ResY=360")
        .arg("-unattended")
        .arg("-NoSplash")
        .creation_flags(0x08000000)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if !config.pack.is_empty() {
        command.arg(format!("-EchoAvatarPack={}", config.pack));
    }
    match command.spawn() {
        Ok(mut child) => {
            use std::os::windows::io::AsRawHandle;
            let tree = match crate::agent_tools::ProcessTree::attach_handle(child.as_raw_handle()) {
                Ok(tree) => tree,
                Err(_) => {
                    stop.store(true, Ordering::SeqCst);
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = server.join();
                    return Err("Could not isolate avatar process lifetime.".into());
                }
            };
            set_status("Loading Unreal avatar…");
            Ok(Companion {
                stop,
                child,
                server: Some(server),
                _tree: tree,
            })
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            let _ = server.join();
            Err("Could not launch the Unreal companion.".into())
        }
    }
}

#[cfg(windows)]
pub(crate) fn private_pipe(
    name: &str,
) -> Result<tokio::net::windows::named_pipe::NamedPipeServer, String> {
    use windows::{
        core::{PCWSTR, PWSTR},
        Win32::{
            Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL},
            Security::{
                Authorization::{
                    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                },
                GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
                TOKEN_QUERY, TOKEN_USER,
            },
            System::Threading::{GetCurrentProcess, OpenProcessToken},
        },
    };
    // The protected DACL grants access only to the process's actual user SID.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|_| "Could not identify avatar user.")?;
        let mut size = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut size);
        let mut storage = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        let result = GetTokenInformation(
            token,
            TokenUser,
            Some(storage.as_mut_ptr().cast()),
            size,
            &mut size,
        );
        let _ = CloseHandle(token);
        result.map_err(|_| "Could not read avatar user.")?;
        let user = &*storage.as_ptr().cast::<TOKEN_USER>();
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text)
            .map_err(|_| "Could not secure avatar transport.")?;
        let sid = text
            .to_string()
            .map_err(|_| "Invalid avatar user identity.");
        let _ = LocalFree(HLOCAL(text.0 as _));
        let descriptor: Vec<u16> = format!("D:P(A;;GA;;;{})", sid?)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut security = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(descriptor.as_ptr()),
            1,
            &mut security,
            None,
        )
        .map_err(|_| "Could not secure avatar transport.")?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security.0,
            bInheritHandle: false.into(),
        };
        let pipe = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                (&attributes as *const SECURITY_ATTRIBUTES)
                    .cast_mut()
                    .cast(),
            );
        let _ = LocalFree(HLOCAL(security.0));
        pipe.map_err(|_| "Could not create private avatar transport.".into())
    }
}

#[cfg(windows)]
async fn serve(
    name: String,
    stop: std::sync::Arc<AtomicBool>,
    ready: std::sync::mpsc::SyncSender<Result<(), String>>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut pipe = match private_pipe(&name) {
        Ok(pipe) => pipe,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    while !stop.load(Ordering::SeqCst) {
        if tokio::time::timeout(std::time::Duration::from_millis(100), pipe.connect())
            .await
            .is_err()
        {
            continue;
        }
        let handshake = async {
            let size = pipe.read_u32_le().await? as usize;
            if size > 1024 {
                return Err(std::io::Error::other("Invalid avatar handshake"));
            }
            let mut bytes = vec![0u8; size];
            pipe.read_exact(&mut bytes).await?;
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            if value["version"] != PROTOCOL || value["type"] != "hello" {
                return Err(std::io::Error::other("Avatar protocol mismatch"));
            }
            Ok::<_, std::io::Error>(())
        };
        if !matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), handshake).await,
            Ok(Ok(()))
        ) {
            let _ = pipe.disconnect();
            set_status("Unreal avatar protocol mismatch");
            continue;
        }
        while AUDIO.1.try_recv().is_ok() {}
        CONNECTED.store(true, Ordering::SeqCst);
        set_status("Unreal transport connected · awaiting character readiness");
        let mut previous = Vec::new();
        let mut responses = Vec::<u8>::new();
        while !stop.load(Ordering::SeqCst) {
            let mut buffer = [0u8; 4096];
            match pipe.try_read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => responses.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => break,
            }
            if responses.len() >= 4 {
                let size = u32::from_le_bytes(responses[..4].try_into().unwrap()) as usize;
                if size > 4096 {
                    break;
                }
                if responses.len() >= size + 4 {
                    if let Ok(message) =
                        serde_json::from_slice::<serde_json::Value>(&responses[4..size + 4])
                    {
                        if message["version"] == PROTOCOL && message["type"] == "ready" {
                            let rendered = message["rendered"].as_bool().unwrap_or(false);
                            RENDER_READY.store(rendered, Ordering::SeqCst);
                            set_status(
                                if rendered && message["facial_frames"].as_u64().unwrap_or(0) > 0 {
                                    "Unreal character rendering · facial animation ready"
                                } else if rendered {
                                    "Unreal character rendering · awaiting facial animation"
                                } else {
                                    "Unreal connected · compatible character pack required"
                                },
                            );
                        }
                    }
                    responses.drain(..size + 4);
                }
            }
            // The Rust bubble owns text; a 3D renderer needs state and PCM only.
            let state = crate::assistant_presentation::runtime_snapshot();
            let frame =
                encode(&serde_json::json!({"version":PROTOCOL,"type":"state","data":state}));
            if let Ok(frame) = frame {
                if frame != previous {
                    if !matches!(
                        tokio::time::timeout(
                            std::time::Duration::from_millis(100),
                            pipe.write_all(&frame)
                        )
                        .await,
                        Ok(Ok(()))
                    ) {
                        break;
                    }
                    previous = frame;
                }
            }
            let clock = *CLOCK.lock().unwrap();
            let frame = encode(&serde_json::json!({"version":PROTOCOL,"type":"clock","generation":clock.0,"micros":clock.1})).unwrap();
            if !matches!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(100),
                    pipe.write_all(&frame)
                )
                .await,
                Ok(Ok(()))
            ) {
                break;
            }
            while let Ok(audio) = AUDIO.1.try_recv() {
                if audio.generation != crate::assistant_voice::playback_generation() {
                    continue;
                }
                if let Ok(frame) =
                    encode(&serde_json::json!({"version":PROTOCOL,"type":"audio","data":audio}))
                {
                    if !matches!(
                        tokio::time::timeout(
                            std::time::Duration::from_millis(100),
                            pipe.write_all(&frame)
                        )
                        .await,
                        Ok(Ok(()))
                    ) {
                        break;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        CONNECTED.store(false, Ordering::SeqCst);
        RENDER_READY.store(false, Ordering::SeqCst);
        let _ = pipe.disconnect();
        if !stop.load(Ordering::SeqCst) {
            set_status("Avatar disconnected · workspace still available");
        }
    }
}

#[cfg(not(windows))]
pub fn launch(_: &crate::assistant_presentation::AvatarConfig) -> Result<Companion, String> {
    Err("Unreal companion currently requires Windows.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frame_is_length_prefixed_and_bounded() {
        let frame = encode(&serde_json::json!({"type":"hello","version":PROTOCOL})).unwrap();
        assert_eq!(
            u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize,
            frame.len() - 4
        );
        assert!(encode(&"x".repeat(MAX_FRAME)).is_err());
    }
    #[test]
    fn audio_queue_is_bounded_and_does_not_wait() {
        let (tx, rx) = crossbeam_channel::bounded(12);
        for _ in 0..12 {
            tx.try_send(1).unwrap();
        }
        assert!(tx.try_send(1).is_err());
        assert_eq!(rx.len(), 12);
    }
    #[test]
    fn long_utterance_animation_is_paced_and_preserves_its_tail() {
        let expected = (0..240_000).map(|i| i as f32).collect::<Vec<_>>();
        let spans = std::sync::Arc::new(Mutex::new(Vec::new()));
        let capture = spans.clone();
        let mut source = paced_source(
            expected.clone(),
            std::num::NonZeroU32::new(24_000).unwrap(),
            move |offset, samples| {
                capture.lock().unwrap().push((offset, samples));
            },
            |_| {},
        );
        assert_eq!(source.next(), Some(0.0));
        assert!(
            spans
                .lock()
                .unwrap()
                .iter()
                .map(|(_, s)| s.len())
                .sum::<usize>()
                <= 1920
        );
        assert_eq!(source.count(), expected.len() - 1);
        let spans = spans.lock().unwrap();
        let mut observed = Vec::new();
        for (offset, span) in spans.iter() {
            assert_eq!(*offset, observed.len());
            observed.extend_from_slice(span);
        }
        assert_eq!(observed, expected);
    }
}
