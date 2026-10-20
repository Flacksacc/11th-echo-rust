//! One curated component catalog and installer shared by setup and Settings.
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Choice {
    pub id: String,
    pub page: i32,
    pub title: String,
    pub description: String,
    pub tradeoff: String,
    pub download: String,
    pub selected: bool,
    pub installed: bool,
}
pub fn catalog() -> Vec<Choice> {
    [("parakeet_v2",0,"CPU transcription — Parakeet v2","Private English transcription without a GPU.","CPU processing is slower than GPU transcription; English only.","482 MB",true),
     ("punctuation",0,"Offline punctuation and capitalization","Adds sentence marks and casing locally.","English only; another processing step.","31 MB",true),
     ("parakeet_v3",1,"Parakeet v3","CPU transcription with 25-language support.","Additional model storage and CPU processing.","487 MB",false),
     ("parakeet_ultra",1,"Parakeet Ultra","GPU transcription with 25-language support.","Requires a compatible NVIDIA GPU and a large private GPU runtime.","1.26 GB weights plus runtime",false),
     ("pocket",2,"Pocket TTS — standard voice","Local streaming assistant speech using Alba; no API key.","Extra model storage and CPU usage.","236 MB",false),
     ("pocket_clone",2,"Custom voice import","Convert recordings into reusable voice prompts.","Requires access to gated weights and a Hugging Face token.","236 MB; replaces standard weights",false),
     ("laya",2,"Laya decision model","Fast local routing between assistant branches.","Routes decisions; does not write answers. Runs through a private Ollaya service.","Approximately 900 MB",false),
     ("browser",3,"Browser automation","Use websites through a separate managed browser.","Downloads private Node and Chromium runtimes; separate from your usual browser profile.","Approximately 350 MB",false),
     ("desktop",3,"Windows desktop control","Inspect and operate applications on this Windows desktop.","Application support varies; changes require approval.","Cua Driver package",false)]
        .into_iter().map(|(id,page,title,description,tradeoff,download,selected)|Choice{id:id.into(),page,title:title.into(),description:description.into(),tradeoff:tradeoff.into(),download:download.into(),selected,installed:false}).collect()
}
pub fn root() -> PathBuf {
    crate::settings::settings_path().with_file_name("components")
}
pub type Progress = Arc<dyn Fn(String) + Send + Sync>;
pub fn downloaded(id: &str) -> bool {
    let local = dirs_next::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("11th_echo");
    match id {
        "parakeet_v2" => local.join("models/parakeet-tdt-0.6b-v2-int8").is_dir(),
        "parakeet_v3" => local.join("models/parakeet-tdt-0.6b-v3-int8").is_dir(),
        "parakeet_ultra" => local.join("ultra").is_dir(),
        "punctuation" => crate::post_processing::punctuation_model_available(),
        "pocket" => crate::assistant_voice::installed(),
        "browser" => root()
            .join("browser/node_modules/@playwright/mcp/cli.js")
            .is_file(),
        "desktop" => find_executable(&root().join("desktop"), "cua-driver.exe").is_some(),
        "laya" => find_executable(&root().join("ollaya"), "ollaya.exe").is_some(),
        _ => false,
    }
}
pub fn fresh_settings() -> crate::settings::AppSettings {
    let mut settings = crate::settings::AppSettings {
        transcription_provider: crate::transcription::LOCAL_SHERPA_PROVIDER_ID.into(),
        ..Default::default()
    };
    settings.local_sherpa.model = crate::transcription::LocalModel::ParakeetTdtV2;
    settings.normalize_transcription_settings();
    settings.pipelines = Some(crate::pipelines::migrate(&settings));
    crate::graph_ui::upgrade(&mut settings);
    settings
}

pub async fn install(id: &str, token: &str, progress: Progress) -> Result<(), String> {
    progress(format!("Installing {id}…"));
    match id {
        "parakeet_v2"|"parakeet_v3"|"parakeet_ultra"=>{let model=match id{"parakeet_v3"=>crate::transcription::LocalModel::ParakeetTdtV3,"parakeet_ultra"=>crate::transcription::LocalModel::ParakeetUltra,_=>crate::transcription::LocalModel::ParakeetTdtV2};crate::transcription::download_local_models(model,move|percent,text|progress(format!("{:.0}% · {text}",percent*100.))).await},
        "punctuation"=>crate::post_processing::download_punctuation_model(move|percent,text|progress(format!("{:.0}% · {text}",percent*100.))).await,
        "pocket"=>crate::assistant_voice::install_standard().await,
        "pocket_clone"=>crate::assistant_voice::install(token).await,
        "desktop"=>install_zip("desktop","https://github.com/trycua/cua/releases/download/cua-driver-rs-v0.34.0/cua-driver-rs-0.34.0-windows-x86_64-binary.zip","bcc520e50861c7092cf775846fec76ae386d7dcd6b5b408608b0ea4423a8b888").await,
        "browser"=>{
            install_zip("node","https://nodejs.org/dist/v24.21.0/node-v24.21.0-win-x64.zip","158f7685b44de51f6c0df1d153526cbcd3e1bc739a8dfc607721cef75de9e541").await?;
            let dir=root().join("browser");tokio::fs::create_dir_all(&dir).await.map_err(|_|"Could not create browser runtime.")?;
            tokio::fs::write(dir.join("package.json"),include_bytes!("../assets/browser-tools/package.json")).await.map_err(|_|"Could not save browser manifest.")?;
            tokio::fs::write(dir.join("package-lock.json"),include_bytes!("../assets/browser-tools/package-lock.json")).await.map_err(|_|"Could not save browser lockfile.")?;
            let node=node_executable();let npm=node.parent().ok_or("Node runtime is incomplete.")?.join("node_modules/npm/bin/npm-cli.js");
            progress("Installing verified, locked browser packages…".into());
            run(&node,&[npm.to_string_lossy().into(),"ci".into(),"--ignore-scripts".into(),"--no-audit".into(),"--no-fund".into()],Some(&dir)).await?;
            progress("Installing Chromium…".into());
            tokio::fs::create_dir_all(root().join("chromium")).await.map_err(|_|"Could not create browser storage.")?;
            install_zip("chromium/chromium-1247","https://storage.googleapis.com/chrome-for-testing-public/155.0.8059.12/win64/chrome-win64.zip","48e66b6d93271398c68564ac8ca081b862cb7b78851ec3eae376a9a8d1941a15").await?;
            tokio::fs::write(root().join("chromium/chromium-1247/INSTALLATION_COMPLETE"),b"").await.map_err(|_|"Could not finish browser installation.")?;Ok(())
        },
        "laya"=>{
            stop_laya();
            install_zip("ollaya","https://github.com/ollaya-dev/ollaya/releases/download/v0.8.0/ollaya-windows-amd64.zip","bb6500f0c73af0e5f68d85cd92aefcf1498ce7728fd6573ae1abcc9ef1aeb828").await?;
            let exe=find_executable(&root().join("ollaya"),"ollaya.exe").ok_or("Ollaya package is incomplete.")?;
            ensure_laya().await?;
            run(&exe,&["pull".into(),"laya:en".into()],None).await?;
            let mut probe=crate::settings::AppSettings::default();register_laya(&mut probe)?;
            crate::system_one::decide(&probe.model_registry.connections[0],"laya:en",crate::system_one::INSTRUCTIONS,"Hello",120).await?;Ok(())
        },
        _=>Err("Unknown optional component.".into())
    }
}
pub fn node_executable() -> PathBuf {
    root().join("node/node-v24.21.0-win-x64/node.exe")
}
pub fn find_executable(dir: &Path, name: &str) -> Option<PathBuf> {
    if dir.join(name).is_file() {
        return Some(dir.join(name));
    }
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        if entry.file_type().ok()?.is_dir() && entry.path().join(name).is_file() {
            return Some(entry.path().join(name));
        }
    }
    None
}
pub fn server(id: &str) -> Result<crate::assistant_mcp::McpServer, String> {
    use crate::assistant_mcp::{McpServer, McpTransport};
    let (command, args) = match id {
        "desktop" => (
            find_executable(&root().join("desktop"), "cua-driver.exe")
                .ok_or("Install desktop control first.")?,
            vec!["mcp".into(), "--direct".into()],
        ),
        "browser" => (
            node_executable(),
            vec![
                root()
                    .join("browser/node_modules/@playwright/mcp/cli.js")
                    .to_string_lossy()
                    .into(),
                "--isolated".into(),
                "--browser".into(),
                "chrome".into(),
                "--executable-path".into(),
                root()
                    .join("chromium/chromium-1247/chrome-win64/chrome.exe")
                    .to_string_lossy()
                    .into(),
                "--headless".into(),
                "--output-dir".into(),
                crate::artifacts::root().to_string_lossy().into(),
            ],
        ),
        _ => return Err("This component does not provide an MCP server.".into()),
    };
    let mut environment = std::collections::BTreeMap::new();
    if id == "browser" {
        environment.insert(
            "PLAYWRIGHT_BROWSERS_PATH".into(),
            root().join("chromium").to_string_lossy().into(),
        );
    }
    Ok(McpServer {
        id: format!("echo-{id}"),
        name: if id == "browser" {
            "Browser"
        } else {
            "Windows desktop"
        }
        .into(),
        enabled: true,
        transport: McpTransport::Stdio {
            command: command.to_string_lossy().into(),
            args,
        },
        discovered_tools: vec![],
        environment,
        headers: Default::default(),
    })
}
async fn install_zip(id: &str, url: &str, hash: &str) -> Result<(), String> {
    let dir = root();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|_| "Could not create component storage.")?;
    let staging = dir.join(format!("{id}-{}", crate::pipelines::new_id()));
    tokio::fs::create_dir(&staging)
        .await
        .map_err(|_| "Could not stage component.")?;
    let response = reqwest::get(url)
        .await
        .map_err(|_| "Component download failed.")?
        .error_for_status()
        .map_err(|_| "Component download was rejected.")?;
    if response.url().scheme() != "https" {
        return Err("Component download left HTTPS.".into());
    }
    let mut stream = response.bytes_stream();
    let mut data = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "Component download interrupted.")?;
        if data.len() + chunk.len() > 1024 * 1024 * 1024 {
            return Err("Component archive exceeded the size limit.".into());
        }
        data.extend_from_slice(&chunk);
    }
    if format!("{:x}", Sha256::digest(&data)) != hash {
        return Err("Component checksum did not match.".into());
    }
    let target = staging.clone();
    tokio::task::spawn_blocking(move || extract(&data, &target))
        .await
        .map_err(|_| "Component extraction failed.")??;
    let destination = dir.join(id);
    let backup = dir.join(format!("{id}-previous-{}", crate::pipelines::new_id()));
    if destination.exists() {
        tokio::fs::rename(&destination, &backup)
            .await
            .map_err(|_| "Close component tools before repairing them.")?;
    }
    if tokio::fs::rename(&staging, &destination).await.is_err() {
        if backup.exists() {
            let _ = tokio::fs::rename(&backup, &destination).await;
        }
        return Err("Could not commit component installation.".into());
    }
    // The previous immutable version is retained for recovery, not recursively removed.
    Ok(())
}
fn extract(bytes: &[u8], target: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|_| "Invalid component archive.")?;
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|_| "Invalid archive entry.")?;
        let relative = file.enclosed_name().ok_or("Unsafe archive path.")?;
        if relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
            || relative.to_string_lossy().contains(':')
            || file.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
        {
            return Err("Unsafe component archive entry.".into());
        }
        total = total.saturating_add(file.size());
        if total > 2 * 1024 * 1024 * 1024 {
            return Err("Expanded component exceeded the size limit.".into());
        }
        let path = target.join(relative);
        if file.is_dir() {
            std::fs::create_dir_all(path).map_err(|_| "Could not create component directory.")?;
        } else {
            std::fs::create_dir_all(path.parent().ok_or("Invalid archive path.")?)
                .map_err(|_| "Could not create component directory.")?;
            let mut output =
                std::fs::File::create(path).map_err(|_| "Could not create component file.")?;
            std::io::copy(&mut file, &mut output).map_err(|_| "Could not extract component.")?;
        }
    }
    Ok(())
}
async fn run(exe: &Path, args: &[String], dir: Option<&Path>) -> Result<(), String> {
    let mut process = tokio::process::Command::new(exe);
    process
        .args(args)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(dir) = dir {
        process.current_dir(dir);
    }
    let mut paths = vec![exe.parent().ok_or("Invalid helper path.")?.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    process
        .env(
            "PATH",
            std::env::join_paths(paths).map_err(|_| "Invalid runtime PATH.")?,
        )
        .env("PLAYWRIGHT_BROWSERS_PATH", root().join("chromium"))
        .env("OLLAYA_HOST", "127.0.0.1:11436")
        .env("OLLAYA_MODELS", root().join("laya-models"));
    #[cfg(windows)]
    process.creation_flags(0x08000000);
    let status = process
        .status()
        .await
        .map_err(|_| "Could not run component installer.")?;
    if !status.success() {
        return Err("Component installation failed; retry in Settings.".into());
    }
    Ok(())
}

struct LayaProcess {
    child: std::process::Child,
    #[cfg(windows)]
    _tree: crate::agent_tools::ProcessTree,
}
static LAYA: std::sync::Mutex<Option<LayaProcess>> = std::sync::Mutex::new(None);
pub async fn ensure_laya() -> Result<(), String> {
    {
        let mut owned = LAYA.lock().unwrap();
        if !owned
            .as_mut()
            .is_some_and(|process| matches!(process.child.try_wait(), Ok(None)))
        {
            if std::net::TcpStream::connect_timeout(
                &"127.0.0.1:11436".parse().unwrap(),
                std::time::Duration::from_millis(100),
            )
            .is_ok()
            {
                return Err("Echo's private decision-model port 11436 is already in use.".into());
            }
            let exe = find_executable(&root().join("ollaya"), "ollaya.exe")
                .ok_or("Install Laya in Components first.")?;
            let mut command = std::process::Command::new(exe);
            command
                .arg("serve")
                .env("OLLAYA_HOST", "127.0.0.1:11436")
                .env("OLLAYA_MODELS", root().join("laya-models"))
                .env("OLLAYA_DEVICE", "cpu")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000);
            }
            let mut child = command
                .spawn()
                .map_err(|_| "Could not start Echo's decision-model service.")?;
            #[cfg(windows)]
            let tree = {
                use std::os::windows::io::AsRawHandle;
                match crate::agent_tools::ProcessTree::attach_handle(child.as_raw_handle()) {
                    Ok(tree) => tree,
                    Err(error) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(error);
                    }
                }
            };
            *owned = Some(LayaProcess {
                child,
                #[cfg(windows)]
                _tree: tree,
            });
        }
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(1))
        .build()
        .map_err(|_| "Could not initialize decision service.")?;
    for _ in 0..60 {
        if client
            .get("http://127.0.0.1:11436/api/tags")
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err("Echo's decision-model service did not become ready.".into())
}
pub fn stop_laya() {
    if let Some(mut process) = LAYA.lock().unwrap().take() {
        let _ = process.child.kill();
        let _ = process.child.wait();
    }
}
pub fn register_laya(settings: &mut crate::settings::AppSettings) -> Result<(), String> {
    let connection = crate::models::ModelConnection {
        id: "echo-laya".into(),
        provider: crate::system_one::PROVIDER.into(),
        endpoint: "http://127.0.0.1:11436".into(),
        api_version: String::new(),
        custom_header_name: String::new(),
        custom_header_value: String::new(),
        api_key: String::new(),
        allow_lan_http: false,
    };
    if let Some(existing) = settings
        .model_registry
        .connections
        .iter()
        .find(|c| c.id == connection.id)
    {
        if existing != &connection {
            return Err(
                "The Echo Laya connection was customized. Review it in Models & connections."
                    .into(),
            );
        }
    } else {
        settings.model_registry.connections.push(connection);
    }
    if !settings
        .model_registry
        .models
        .iter()
        .any(|m| m.id == "echo-laya-router")
    {
        let mut model = crate::models::ConfiguredModel::new(
            "echo-laya".into(),
            "Laya · Echo local decision".into(),
            "laya:en".into(),
        );
        model.id = "echo-laya-router".into();
        model.instructions = crate::system_one::INSTRUCTIONS.into();
        model.verified = true;
        settings.model_registry.models.push(model);
    }
    Ok(())
}

pub fn remove(id: &str) -> Result<(), String> {
    if id == "laya" {
        stop_laya();
    }
    let data = crate::settings::settings_path()
        .parent()
        .ok_or("Invalid Echo data directory.")?
        .to_path_buf();
    let local = dirs_next::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("11th_echo");
    let path = match id {
        "desktop" | "browser" => root().join(id),
        "parakeet_v2" => local.join("models/parakeet-tdt-0.6b-v2-int8"),
        "parakeet_v3" => local.join("models/parakeet-tdt-0.6b-v3-int8"),
        "parakeet_ultra" => local.join("ultra"),
        "punctuation" => local.join("models/punctuation/edge-punct-casing"),
        "pocket" | "pocket_clone" => crate::assistant_voice::asset_dir(),
        "laya" => root().join("ollaya"),
        _ => return Err("Unknown component.".into()),
    };
    let mut paths = vec![path];
    if id == "browser" {
        paths.extend([root().join("node"), root().join("chromium")]);
    }
    if id == "laya" {
        paths.push(root().join("laya-models"));
    }
    let paths = paths.into_iter().filter(|p| p.exists()).collect::<Vec<_>>();
    if paths.is_empty() {
        return Ok(());
    }
    let roots = [data, local]
        .into_iter()
        .filter_map(|path| path.canonicalize().ok())
        .collect::<Vec<_>>();
    let mut actual_paths = vec![];
    for path in paths {
        let actual = path
            .canonicalize()
            .map_err(|_| "Could not resolve component directory.")?;
        if !roots
            .iter()
            .any(|root| actual.starts_with(root) && actual != *root)
        {
            return Err("Component path is outside Echo's data directory.".into());
        }
        actual_paths.push(actual);
    }
    if id == "pocket" || id == "pocket_clone" {
        crate::assistant_voice::unload();
    }
    if id == "punctuation" {
        crate::post_processing::unload_punctuation();
    }
    crate::transcription::shutdown_local_engine();
    for actual in actual_paths {
        std::fs::remove_dir_all(actual)
            .map_err(|_| "Component is in use. Close its tools and retry.".to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_core_is_selected_by_default() {
        assert_eq!(
            catalog()
                .iter()
                .filter(|c| c.selected)
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            ["parakeet_v2", "punctuation"]
        );
    }
    #[test]
    fn fresh_setup_graph_uses_cpu_transcription_and_local_punctuation() {
        let settings = fresh_settings();
        assert_eq!(
            settings.transcription_provider,
            crate::transcription::LOCAL_SHERPA_PROVIDER_ID
        );
        let graph = settings
            .pipelines
            .as_ref()
            .unwrap()
            .active()
            .unwrap()
            .graph
            .as_ref()
            .unwrap();
        assert!(
            matches!(&graph.transcription().unwrap().kind,crate::pipelines::StepKind::Transcription{provider,local,..} if provider==crate::transcription::LOCAL_SHERPA_PROVIDER_ID && local.model==crate::transcription::LocalModel::ParakeetTdtV2)
        );
        assert!(graph.nodes.iter().any(|node| node
            .step()
            .is_some_and(|step| matches!(step.kind, crate::pipelines::StepKind::Punctuation(_)))));
    }
    #[test]
    fn archive_parent_paths_are_rejected() {
        let mut bytes = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut bytes);
            writer
                .start_file("../outside", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.finish().unwrap();
        }
        assert!(extract(bytes.get_ref(), Path::new("target/unused-unsafe-archive")).is_err());
    }
}
