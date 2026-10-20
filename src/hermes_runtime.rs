//! Private Hermes/Codex provisioning and owned Windows process lifetime.
use crate::settings::AppSettings;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;

pub fn root() -> PathBuf {
    if let Some(path) = std::env::var_os("ECHO_HERMES_COMPONENT_DIR") {
        return PathBuf::from(path);
    }
    crate::settings::settings_path()
        .with_file_name("components")
        .join("hermes")
}

fn package_bridge(root: &Path) -> Result<(), String> {
    let package = root.join("bridge/echo_bridge");
    std::fs::create_dir_all(&package)
        .map_err(|_| "Could not prepare the Hermes adapter directory.")?;
    for (name, data) in [
        (
            "codex_import.py",
            include_str!("../integration/hermes/echo_bridge/codex_import.py"),
        ),
        (
            "__init__.py",
            include_str!("../integration/hermes/echo_bridge/__init__.py"),
        ),
        (
            "native.py",
            include_str!("../integration/hermes/echo_bridge/native.py"),
        ),
        (
            "history.py",
            include_str!("../integration/hermes/echo_bridge/history.py"),
        ),
        (
            "server.py",
            include_str!("../integration/hermes/echo_bridge/server.py"),
        ),
        (
            "configure.py",
            include_str!("../integration/hermes/echo_bridge/configure.py"),
        ),
        (
            "google_provider.py",
            include_str!("../integration/hermes/echo_bridge/google_provider.py"),
        ),
        (
            "secrets.py",
            include_str!("../integration/hermes/echo_bridge/secrets.py"),
        ),
        (
            "plugin.py",
            include_str!("../integration/hermes/echo_bridge/plugin.py"),
        ),
        (
            "worker_tools.py",
            include_str!("../integration/hermes/echo_bridge/worker_tools.py"),
        ),
        (
            "actor_proxy.py",
            include_str!("../integration/hermes/echo_bridge/actor_proxy.py"),
        ),
        (
            "access.py",
            include_str!("../integration/hermes/echo_bridge/access.py"),
        ),
        (
            "stop_owned.py",
            include_str!("../integration/hermes/echo_bridge/stop_owned.py"),
        ),
        (
            "update_probe.py",
            include_str!("../integration/hermes/echo_bridge/update_probe.py"),
        ),
    ] {
        std::fs::write(package.join(name), data)
            .map_err(|_| "Could not write the Hermes adapter.")?;
    }
    let plugin = root.join("home/plugins/echo_integration");
    std::fs::create_dir_all(&plugin).map_err(|_| "Could not prepare the Hermes plugin.")?;
    std::fs::write(
        plugin.join("__init__.py"),
        "from echo_bridge.plugin import register\n",
    )
    .map_err(|_| "Could not write the Hermes plugin entry point.")?;
    std::fs::write(plugin.join("plugin.yaml"), "name: echo_integration\nversion: 0.1.0\nkind: standalone\ndescription: Echo native task and conversation integration\n")
        .map_err(|_| "Could not write the Hermes plugin manifest.")?;
    let provider = root.join("home/plugins/model-providers/echo-google");
    std::fs::create_dir_all(&provider)
        .map_err(|_| "Could not prepare the Google provider extension.")?;
    std::fs::write(
        provider.join("__init__.py"),
        "from echo_bridge.google_provider import register\nregister()\n",
    )
    .map_err(|_| "Could not write the Google provider entry point.")?;
    std::fs::write(provider.join("plugin.yaml"), "name: echo-google-provider\nkind: model-provider\nversion: 0.1.0\ndescription: Echo connection using the native Hermes Gemini adapter\n")
        .map_err(|_| "Could not write the Google provider manifest.")?;
    Ok(())
}

pub async fn install() -> Result<(), String> {
    let root = root();
    std::fs::create_dir_all(&root)
        .map_err(|_| "Could not create the private Hermes component directory.")?;
    let _lock = runtime_lock(&root)?;
    recover_update(&root).await?;
    let existing = root.join("source/.git").exists() && root.join("home/installs").exists();
    if existing {
        package_bridge(&root)?;
        maintain_locked(&root, "repair").await?;
    }
    let installer = root.join("echo-install.ps1");
    std::fs::write(
        &installer,
        include_bytes!("../integration/hermes/install.ps1"),
    )
    .map_err(|_| "Could not stage the Hermes installer.")?;
    let mut command = tokio::process::Command::new("powershell.exe");
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&installer)
        .arg("-Root")
        .arg(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if existing {
        // Repair Codex as before, without re-running the pinned Hermes bootstrap.
        command.arg("-CodexOnly");
    }
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .map_err(|_| "Could not launch the hidden Hermes installer.")?;
    #[cfg(windows)]
    let _tree = crate::agent_tools::ProcessTree::attach_pid(
        child.id().ok_or("Installer did not return a process ID.")?,
    )?;
    let status = child
        .wait()
        .await
        .map_err(|_| "Hermes installation was interrupted.")?;
    if !status.success() {
        return Err("Hermes installation failed. Check the download connection and use Install / repair again.".into());
    }
    package_bridge(&root)?;
    Ok(())
}

/// Caller holds the same exclusive lock as the gateway. Recovery uses the saved
/// interpreter, so a broken new dependency selection cannot strand the user.
async fn recover_update(root: &Path) -> Result<(), String> {
    if root.join("update-journal.json").exists() {
        maintain_locked(root, "recover").await?;
    }
    Ok(())
}

pub async fn update() -> Result<String, String> {
    let root = root();
    if !root.join("source/.git").exists() {
        return Err("Install Hermes before updating it.".into());
    }
    let _lock = runtime_lock(&root)?;
    recover_update(&root).await?;
    package_bridge(&root)?;
    maintain_locked(&root, "update").await?;
    Ok(
        "Hermes updated to the latest main version. Conversations and settings were preserved."
            .into(),
    )
}

async fn maintain_locked(root: &Path, mode: &str) -> Result<(), String> {
    let script = root.join("echo-update.py");
    std::fs::write(
        &script,
        include_bytes!("../integration/hermes/echo_bridge/update.py"),
    )
    .map_err(|_| "Could not prepare the Hermes updater.")?;
    let python = if root.join("update-journal.json").exists() {
        let bytes = std::fs::read(root.join("update-journal.json"))
            .map_err(|_| "Could not read Hermes recovery information.")?;
        if bytes.len() > 4096 {
            return Err("Hermes recovery information is invalid.".into());
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "Hermes recovery information is invalid.")?;
        let relative = Path::new(
            value["python"]
                .as_str()
                .ok_or("Recovery interpreter is missing.")?,
        );
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err("Hermes recovery interpreter is invalid.".into());
        }
        let path = root
            .join(relative)
            .canonicalize()
            .map_err(|_| "The previous Hermes interpreter is unavailable.")?;
        if !path.starts_with(
            root.canonicalize()
                .map_err(|_| "Hermes folder is unavailable.")?,
        ) {
            return Err("Hermes recovery interpreter is outside its managed folder.".into());
        }
        path
    } else {
        match managed_python(root) {
            Ok(python) => python,
            Err(error) if mode == "repair" => {
                let canonical = root
                    .canonicalize()
                    .map_err(|_| "Hermes folder is unavailable.")?;
                std::fs::read_dir(root.join("home/tools"))
                    .map_err(|_| error.clone())?
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_name().to_string_lossy().starts_with("python-"))
                    .map(|entry| entry.path().join("python.exe"))
                    .find(|path| {
                        path.canonicalize()
                            .is_ok_and(|resolved| resolved.starts_with(&canonical))
                    })
                    .ok_or(error)?
            }
            Err(error) => return Err(error),
        }
    };
    let mut command = tokio::process::Command::new(&python);
    command
        .arg(script)
        .arg(root)
        .arg(mode)
        .current_dir(root)
        .env("HERMES_HOME", root.join("home"))
        .env("CODEX_HOME", root.join("codex-home"))
        .env("PYTHONPATH", root.join("bridge"))
        .env("PATH", managed_path(root, &python)?)
        .env_remove("__HERMES_ACTIVATED")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .map_err(|_| "Could not launch the Hermes updater.")?;
    #[cfg(windows)]
    let _tree = crate::agent_tools::ProcessTree::attach_pid(
        child.id().ok_or("Updater did not return a process ID.")?,
    )?;
    let status = child
        .wait()
        .await
        .map_err(|_| "Hermes update was interrupted. Echo will recover it before reconnecting.")?;
    if status.code() == Some(2) {
        let incompatible = std::fs::read(root.join("update-failure.json"))
            .ok()
            .filter(|bytes| bytes.len() <= 256)
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|value| value["stage"] == "compatibility");
        if incompatible {
            return Err("The latest Hermes version failed Echo's compatibility checks. The previous installation was restored; use Connect to resume.".into());
        }
        return Err("Hermes update failed or was incompatible with Echo. The previous installation was restored; use Connect to resume.".into());
    }
    if !status.success() {
        return Err("Hermes maintenance failed. Check connectivity, free disk space and that tasks are idle. Use Connect to recover an interrupted update.".into());
    }
    Ok(())
}

fn runtime_lock(root: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    options
        .open(root.join("runtime.lock"))
        .map_err(|_| "Another Echo process is already using this Hermes runtime.".into())
}

fn managed_python(root: &Path) -> Result<PathBuf, String> {
    let mut receipts = std::fs::read_dir(root.join("home/installs"))
        .map_err(|_| "Install Hermes before connecting.")?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("facts.json"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    receipts.sort_by_key(|path| std::fs::metadata(path).and_then(|m| m.modified()).ok());
    for receipt in receipts.into_iter().rev() {
        let value: Value = serde_json::from_slice(
            &std::fs::read(receipt).map_err(|_| "Could not read the Hermes install receipt.")?,
        )
        .map_err(|_| "Hermes install receipt is invalid. Use Install / repair.")?;
        if let Some(environment) = value
            .pointer("/packages/venv/environment")
            .and_then(Value::as_str)
        {
            let python = Path::new(environment).join("Scripts/python.exe");
            let resolved_root = root
                .canonicalize()
                .map_err(|_| "Hermes component directory is unavailable.")?;
            let resolved = python
                .canonicalize()
                .map_err(|_| "Hermes Python runtime is missing. Use Install / repair.")?;
            if !resolved.starts_with(&resolved_root) {
                return Err(
                    "Hermes receipt points outside its managed component directory.".into(),
                );
            }
            return Ok(python);
        }
    }
    Err("Hermes dependency environment is missing. Use Install / repair.".into())
}

fn managed_path(root: &Path, python: &Path) -> Result<std::ffi::OsString, String> {
    let canonical = root
        .canonicalize()
        .map_err(|_| "Hermes runtime folder is unavailable.")?;
    let store = root.join("home/tools");
    let mut directories = vec![
        python
            .parent()
            .ok_or("Python has no runtime directory.")?
            .to_path_buf(),
        root.join("codex-bin"),
    ];
    if let Ok(bytes) = std::fs::read(store.join("facts.json")) {
        let receipt: Value =
            serde_json::from_slice(&bytes).map_err(|_| "Hermes tool receipt is invalid.")?;
        for package in receipt["packages"]
            .as_object()
            .into_iter()
            .flat_map(|packages| packages.values())
        {
            for entry in package
                .pointer("/env/PATH")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                directories.push(PathBuf::from(
                    entry.replace("{{store}}", &store.to_string_lossy()),
                ));
            }
        }
    }
    // Upstream bootstrap Git precedes the package-manager receipt.
    for entry in std::fs::read_dir(&store)
        .map_err(|_| "Hermes private tools are missing.")?
        .filter_map(Result::ok)
    {
        if entry.file_name().to_string_lossy().starts_with("git-")
            && entry.path().join("cmd/git.exe").is_file()
        {
            directories.push(entry.path().join("cmd"));
        }
    }
    directories.retain(|directory| directory.is_dir());
    for directory in &directories {
        if !directory
            .canonicalize()
            .map_err(|_| "A managed tool folder is unavailable.")?
            .starts_with(&canonical)
        {
            return Err("A Hermes tool receipt points outside its private runtime.".into());
        }
    }
    directories.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(directories)
        .map_err(|_| "Could not compose the private Hermes tool path.".into())
}

fn write_config(root: &Path, settings: &AppSettings) -> Result<(), String> {
    let (model, connection) = settings
        .model_registry
        .find(&settings.hermes.fast_model_id)?;
    crate::models::validate_connection(connection)?;
    if !model.verified {
        return Err("Verify the selected conversation model in Models & connections first.".into());
    }
    let endpoint = if !connection.endpoint.trim().is_empty() {
        connection.endpoint.clone()
    } else {
        match connection.provider.as_str() {
            "openai" => "https://api.openai.com/v1".into(),
            "anthropic" => "https://api.anthropic.com".into(),
            // Keep the same native Google API surface as Echo's existing
            // Gemini provider. Hermes selects its native streaming adapter
            // from this endpoint; the OpenAI compatibility surface can reject
            // credentials that work with generateContent.
            "gemini" => "https://generativelanguage.googleapis.com/v1beta".into(),
            "openrouter" => "https://openrouter.ai/api/v1".into(),
            _ => {
                return Err(
                    "Set an endpoint for this conversation model in Models & connections.".into(),
                )
            }
        }
    };
    let transport = match connection.provider.as_str() {
        "anthropic" => "anthropic_messages",
        _ => "chat_completions",
    };
    let extra = model.additional_params.clone().unwrap_or_else(|| json!({}));
    if connection.provider == "gemini" {
        validate_google_parameters(&extra)?;
    }
    let mut provider = json!({"base_url":endpoint,"transport":transport,"key_env":"ECHO_FAST_MODEL_KEY", "extra_body":extra});
    if !connection.custom_header_name.is_empty() {
        provider["extra_headers"] =
            json!({connection.custom_header_name.clone(): "${env:ECHO_FAST_MODEL_HEADER}"});
    }
    let mut servers = serde_json::Map::new();
    let mut actors = serde_json::Map::new();
    for (index, server) in settings
        .assistant_mcp_servers
        .iter()
        .filter(|server| server.enabled)
        .enumerate()
    {
        server.validate()?;
        let mut entry = match &server.transport {
            crate::assistant_mcp::McpTransport::Stdio { command, args } => {
                json!({"command":command,"args":args})
            }
            crate::assistant_mcp::McpTransport::Http { url } => json!({"url":url}),
        };
        // Hermes asks for approval for write-capable tools on an untrusted
        // server. Credentials are environment references, never staged values.
        entry["trust"] = json!("untrusted");
        entry["env"] = Value::Object(
            server
                .environment
                .keys()
                .enumerate()
                .map(|(n, key)| {
                    (
                        key.clone(),
                        json!(format!("${{env:ECHO_MCP_{index}_ENV_{n}}}")),
                    )
                })
                .collect(),
        );
        entry["headers"] = Value::Object(
            server
                .headers
                .keys()
                .enumerate()
                .map(|(n, key)| {
                    (
                        key.clone(),
                        json!(format!("${{env:ECHO_MCP_{index}_HEADER_{n}}}")),
                    )
                })
                .collect(),
        );
        let managed_actor = ["desktop", "browser"].into_iter().any(|id| {
            crate::components::server(id).is_ok_and(|managed| {
                server.id == managed.id
                    && server.transport == managed.transport
                    && server.environment == managed.environment
                    && server.headers.is_empty()
            })
        });
        if managed_actor {
            actors.insert(index.to_string(), entry);
            entry = json!({"command":managed_python(root)?,
                "args":["-m","echo_bridge.actor_proxy",index.to_string()],
                "env":{"PYTHONPATH":root.join("bridge")},"trust":"untrusted"});
        }
        servers.insert(format!("echo_mcp_{index}"), entry);
    }
    let provider_name = if connection.provider == "gemini" {
        "echo-google"
    } else {
        "custom:echo-fast"
    };
    let config = json!({
        "model": {"default": model.model, "provider": provider_name, "base_url": endpoint,
            "key_env": "ECHO_FAST_MODEL_KEY", "openai_runtime": "auto"},
        "fallback_providers": [], "toolsets": std::iter::once("web".to_owned()).chain(servers.keys().cloned()).collect::<Vec<_>>(),
        "providers": {"echo-fast": provider}, "mcp_servers": servers,
        "echo_worker_model": settings.hermes.worker_model,
        "echo_managed_actors": actors,
        "agent": {"max_turns": null, "agent_cache": {"max_size": 16}},
        // Hermes' native secret source binds the launcher-supplied credentials
        // to this private default profile, never to sibling worker profiles.
        "secrets": {"echo": {"enabled": true}},
        "gateway": {"multiplex_profiles": true},
        "kanban": {"dispatch_in_gateway": true, "dispatch_interval_seconds": 2,
            "dispatch_profiles": []},
        "platforms": {"api_server": {"enabled": true}},
    });
    let home = root.join("home");
    std::fs::create_dir_all(&home).map_err(|_| "Could not prepare Hermes configuration.")?;
    // This file contains routing only. Credentials remain DPAPI-protected in
    // Echo settings and are supplied to the owned runtime via environment.
    let path = home.join("config.yaml");
    if path.is_file() {
        let backup = home.join("config.before-echo.yaml");
        if !backup.exists() {
            std::fs::copy(&path, backup).map_err(|_| "Could not back up Hermes configuration.")?;
        }
    }
    std::fs::write(
        root.join("echo-config.json"),
        serde_json::to_vec_pretty(&config).unwrap(),
    )
    .map_err(|_| "Could not stage Hermes routing configuration.".to_owned())
}

pub struct Runtime {
    children: Vec<tokio::process::Child>,
    #[cfg(windows)]
    trees: Vec<crate::agent_tools::ProcessTree>,
    _lock: std::fs::File,
}

impl Runtime {
    pub async fn stop_tasks(&self) -> Result<(), String> {
        let root = root();
        let python = managed_python(&root)?;
        let mut command = tokio::process::Command::new(&python);
        command
            .args(["-m", "echo_bridge.stop_owned"])
            .env("HERMES_HOME", root.join("home"))
            .env("PYTHONPATH", root.join("bridge"))
            .env("PATH", managed_path(&root, &python)?)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command
            .spawn()
            .map_err(|_| "Could not start native task cancellation.")?;
        #[cfg(windows)]
        let _tree = crate::agent_tools::ProcessTree::attach_pid(
            child.id().ok_or("Cancellation process has no ID.")?,
        )?;
        let status = tokio::time::timeout(std::time::Duration::from_secs(8), child.wait())
            .await
            .map_err(|_| {
                "Native task cancellation timed out. Owned processes will be terminated on exit."
            })?
            .map_err(|_| "Native task cancellation could not finish.")?;
        if status.success() {
            Ok(())
        } else {
            Err("Some native worker terminations could not be confirmed.".into())
        }
    }
    pub async fn start(settings: &AppSettings) -> Result<Self, String> {
        let root = root();
        let lock = runtime_lock(&root)?;
        recover_update(&root).await?;
        let python = managed_python(&root)?;
        let tool_path = managed_path(&root, &python)?;
        package_bridge(&root)?;
        write_config(&root, settings)?;
        // Hermes correctly scrubs gateway credentials from executor children.
        // The Echo launcher resolves its own connection using a per-user
        // encrypted receipt; never inject the token into Codex's environment.
        let receipt = json!({"echo_executable":std::env::current_exe().map_err(|_|"Could not locate Echo's reminder broker.")?,"token":crate::settings::protect_secret(&settings.hermes.token)?,
                             "bridge_port":settings.hermes.bridge_port});
        let pending = root.join("bridge-connection.pending");
        std::fs::write(
            &pending,
            serde_json::to_vec(&receipt)
                .map_err(|_| "Could not encode the private bridge connection.")?,
        )
        .map_err(|_| "Could not protect the private bridge connection.")?;
        std::fs::rename(pending, root.join("bridge-connection.json"))
            .map_err(|_| "Could not save the private bridge connection.")?;
        let mut configure = tokio::process::Command::new(&python);
        configure
            .args(["-m", "echo_bridge.configure"])
            .current_dir(root.join("source"))
            .env("HERMES_HOME", root.join("home"))
            .env("PYTHONPATH", root.join("bridge"))
            .env(
                "ECHO_EXECUTABLE",
                std::env::current_exe()
                    .map_err(|_| "Could not locate Echo's managed worker launcher.")?,
            )
            .env("CODEX_HOME", root.join("codex-home"))
            .env("PATH", &tool_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(test)]
        if let Some(executable) = std::env::var_os("ECHO_GATE_ECHO_BIN") {
            configure.env("ECHO_EXECUTABLE", executable);
        }
        #[cfg(windows)]
        configure.creation_flags(0x08000000);
        let mut configuring = configure
            .spawn()
            .map_err(|_| "Could not apply the native Hermes configuration.")?;
        #[cfg(windows)]
        let _configuration_tree = crate::agent_tools::ProcessTree::attach_pid(
            configuring.id().ok_or("Configuration process has no ID.")?,
        )?;
        if !configuring
            .wait()
            .await
            .map_err(|_| "Could not apply the native Hermes configuration.")?
            .success()
        {
            return Err("Hermes could not apply its model and worker configuration.".into());
        }
        let (_, connection) = settings
            .model_registry
            .find(&settings.hermes.fast_model_id)?;
        let mut runtime = Self {
            children: Vec::new(),
            #[cfg(windows)]
            trees: Vec::new(),
            _lock: lock,
        };
        // Archive stale jobs before starting the native dispatcher. A stopped
        // or abandoned attempt requires an explicit retry after Echo restarts.
        runtime.stop_tasks().await?;
        for args in [
            vec!["-m", "hermes_cli.main", "gateway", "run", "--no-supervise"],
            vec!["-m", "echo_bridge.server"],
        ] {
            let mut command = tokio::process::Command::new(&python);
            command
                .args(args)
                .current_dir(root.join("source"))
                .env("HERMES_HOME", root.join("home"))
                .env("CODEX_HOME", root.join("codex-home"))
                .env("ECHO_CODEX_PROXY", "1")
                .env("PATH", &tool_path)
                .env("PYTHONPATH", root.join("bridge"))
                .env("API_SERVER_KEY", &settings.hermes.token)
                .env("API_SERVER_HOST", "127.0.0.1")
                .env("API_SERVER_PORT", settings.hermes.gateway_port.to_string())
                .env("ECHO_BRIDGE_PORT", settings.hermes.bridge_port.to_string())
                .env("ECHO_FAST_MODEL_KEY", &connection.api_key)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            #[cfg(windows)]
            command.creation_flags(0x08000000);
            command.env("ECHO_FAST_MODEL_HEADER", &connection.custom_header_value);
            for (index, server) in settings
                .assistant_mcp_servers
                .iter()
                .filter(|server| server.enabled)
                .enumerate()
            {
                for (n, value) in server.environment.values().enumerate() {
                    command.env(format!("ECHO_MCP_{index}_ENV_{n}"), value);
                }
                for (n, value) in server.headers.values().enumerate() {
                    command.env(format!("ECHO_MCP_{index}_HEADER_{n}"), value);
                }
            }
            let child = command
                .spawn()
                .map_err(|_| "Could not launch the managed Hermes runtime.")?;
            #[cfg(windows)]
            runtime
                .trees
                .push(crate::agent_tools::ProcessTree::attach_pid(
                    child.id().ok_or("Hermes process has no ID.")?,
                )?);
            runtime.children.push(child);
        }
        let client = crate::hermes::Client::new(&settings.hermes)?;
        for _ in 0..120 {
            if tokio::time::timeout(std::time::Duration::from_secs(1), client.capabilities())
                .await
                .is_ok_and(|result| result.is_ok())
            {
                return Ok(runtime);
            }
            if runtime
                .children
                .iter_mut()
                .any(|child| child.try_wait().ok().flatten().is_some())
            {
                return Err("Hermes exited while loading. Check its model configuration and repair the component.".into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        Err(
            "Hermes is not ready after loading. Check model access and repair the component."
                .into(),
        )
    }

    pub async fn login(
        on_url: std::sync::Arc<dyn Fn(String) + Send + Sync>,
    ) -> Result<String, String> {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
        let root = root();
        let mut command = tokio::process::Command::new(root.join("codex-bin/codex.exe"));
        command
            .args(["login"])
            .env("CODEX_HOME", root.join("codex-home"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        // The standard command opens a browser and serves its local callback.
        // Keep the temporary authorization link in memory as a manual fallback;
        // never persist stdout, credentials or the OAuth URL in diagnostics.
        let mut child = command
            .spawn()
            .map_err(|_| "Install Codex before logging in.")?;
        #[cfg(windows)]
        let _tree = crate::agent_tools::ProcessTree::attach_pid(
            child.id().ok_or("Codex login has no process ID.")?,
        )?;
        let stdout = child
            .stdout
            .take()
            .ok_or("Codex login has no output pipe.")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("Codex login has no error pipe.")?;
        let drain = |pipe: Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                     on_url: std::sync::Arc<dyn Fn(String) + Send + Sync>| {
            tokio::spawn(async move {
                let mut lines = BufReader::new(pipe.take(64 * 1024)).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    for word in line.split_whitespace() {
                        if let Some(url) = browser_login_url(word) {
                            on_url(url);
                        }
                    }
                }
            })
        };
        let out = drain(Box::new(stdout), on_url.clone());
        let err = drain(Box::new(stderr), on_url);
        let status = tokio::time::timeout(std::time::Duration::from_secs(15 * 60), child.wait())
            .await
            .map_err(|_| "Codex browser login timed out. Use Codex login to try again.")?
            .map_err(|_| "Codex login was interrupted.")?;
        let _ = out.await;
        let _ = err.await;
        if status.success() {
            Ok("Codex login complete in Echo's private account directory.".into())
        } else {
            Err("Codex browser login did not complete. Use Codex login in assistant settings to try again.".into())
        }
    }
}

fn validate_google_parameters(extra: &Value) -> Result<(), String> {
    let fields = extra
        .as_object()
        .ok_or("Gemini additional request parameters must be an object.")?;
    let allowed = [
        "generationConfig",
        "safetySettings",
        "cachedContent",
        "labels",
        "thinking_config",
        "thinkingConfig",
        "response_format",
    ];
    let unsupported = fields
        .keys()
        .filter(|key| !allowed.contains(&key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unsupported.is_empty() {
        return Err(format!("Unsupported Gemini additional request parameters: {}. Put native generation options inside generationConfig.", unsupported.join(", ")));
    }
    if fields
        .get("generationConfig")
        .is_some_and(|value| !value.is_object())
    {
        return Err("Gemini generationConfig must be an object.".into());
    }
    Ok(())
}

fn browser_login_url(word: &str) -> Option<String> {
    if word.len() > 16 * 1024 {
        return None;
    }
    let url = url::Url::parse(word).ok()?;
    (url.scheme() == "https"
        && matches!(url.host_str(), Some("auth.openai.com" | "chatgpt.com"))
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.into())
}

/// Native Hermes invokes model.codex_bin with `app-server`. Keep its transport
/// and lifecycle intact while enforcing Echo's read-only default at the actual
/// Codex permission boundary, after native worker CLI overrides.
pub fn codex_proxy(args: &[String]) -> Result<i32, String> {
    let mut command = std::process::Command::new(root().join("codex-bin/codex.exe"));
    command.args(proxy_arguments(args));
    if args.first().is_some_and(|arg| arg == "app-server") {
        let permission = if let Some(task) = proxy_task(args) {
            let receipt = std::fs::File::open(root().join("bridge-connection.json"))
                .ok()
                .and_then(|file| {
                    use std::io::Read;
                    let mut bytes = Vec::new();
                    file.take(8193).read_to_end(&mut bytes).ok()?;
                    if bytes.len() > 8192 {
                        return None;
                    }
                    serde_json::from_slice::<Value>(&bytes).ok()
                });
            let config = receipt.and_then(|receipt| {
                Some(crate::hermes::Config {
                    token: crate::settings::unprotect_secret(receipt["token"].as_str()?).ok()?,
                    bridge_port: u16::try_from(receipt["bridge_port"].as_u64()?).ok()?,
                    ..Default::default()
                })
            });
            tokio::runtime::Runtime::new().ok().and_then(|runtime| {
                runtime.block_on(async {
                    crate::hermes::Client::new(&config?)
                        .ok()?
                        .worker_permissions(&task)
                        .await
                        .ok()
                })
            })
        } else {
            None
        };
        let approved = permission
            .as_ref()
            .filter(|policy| policy["profile"] == ":workspace")
            .and_then(|policy| policy["workspace"].as_str())
            .and_then(|path| Path::new(path).canonicalize().ok())
            .is_some_and(|path| {
                std::env::current_dir()
                    .ok()
                    .and_then(|cwd| cwd.canonicalize().ok())
                    .is_some_and(|cwd| cwd == path)
            });
        command.args([
            "-c",
            if approved {
                "default_permissions=\":workspace\""
            } else {
                "default_permissions=\":read-only\""
            },
            "-c",
            "sandbox_workspace_write.writable_roots=[]",
            "-c",
            "sandbox_workspace_write.network_access=false",
        ]);
    }
    command
        .env("CODEX_HOME", root().join("codex-home"))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "Managed Codex is not installed.")?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        let _tree = crate::agent_tools::ProcessTree::attach_handle(child.as_raw_handle())?;
        child
            .wait()
            .map(|status| status.code().unwrap_or(1))
            .map_err(|_| "Codex app-server stopped unexpectedly.".into())
    }
    #[cfg(not(windows))]
    child
        .wait()
        .map(|status| status.code().unwrap_or(1))
        .map_err(|_| "Codex app-server stopped unexpectedly.".into())
}

fn proxy_arguments(args: &[String]) -> Vec<String> {
    let mut kept = Vec::with_capacity(args.len());
    let mut index = 0;
    while index < args.len() {
        // The native dispatcher adds its board folder as a writable root.
        // Echo replaces that grant with its explicit workspace policy. Drop
        // the superseded override before Codex parses it: native Windows
        // backslashes may otherwise make its TOML value invalid.
        if args.first().is_some_and(|arg| arg == "app-server")
            && matches!(args[index].as_str(), "-c" | "--config")
            && args
                .get(index + 1)
                .is_some_and(|value| value.starts_with("sandbox_workspace_write.writable_roots="))
        {
            index += 2;
        } else {
            kept.push(args[index].clone());
            index += 1;
        }
    }
    kept
}

fn proxy_task(args: &[String]) -> Option<String> {
    const KEY: &str = "mcp_servers.hermes-tools.env.HERMES_KANBAN_TASK=";
    let task = args
        .windows(2)
        .filter(|pair| pair[0] == "-c")
        .filter_map(|pair| pair[1].strip_prefix(KEY))
        .filter_map(|value| serde_json::from_str::<String>(value).ok())
        .next_back()
        .or_else(|| std::env::var("HERMES_KANBAN_TASK").ok())?;
    (!task.is_empty()
        && task.len() <= 256
        && task
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'))
    .then_some(task)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    #[ignore = "Updates an isolated provisioned Hermes under target; requires network"]
    async fn managed_hermes_update_preserves_state_and_checks_latest() {
        let explicit = std::env::var_os("ECHO_HERMES_COMPONENT_DIR")
            .expect("An isolated component directory is required");
        let component = std::path::PathBuf::from(explicit);
        let target = std::env::current_dir()
            .unwrap()
            .join("target")
            .canonicalize()
            .unwrap();
        assert!(
            component.canonicalize().unwrap().starts_with(target),
            "Never update the user's installation in this test"
        );
        let marker = component.join("home/echo-update-fixture.txt");
        std::fs::write(&marker, "Synthetic user state must survive Hermes updates.").unwrap();
        let python = super::managed_python(&component).unwrap();
        let code = "from hermes_state import SessionDB; db=SessionDB(); db.ensure_session('echo_update_fixture','api_server'); db.append_message('echo_update_fixture','user','Synthetic update preservation message.'); db.close()";
        let status = tokio::process::Command::new(&python)
            .args(["-c", code])
            .current_dir(component.join("source"))
            .env("HERMES_HOME", component.join("home"))
            .status()
            .await
            .unwrap();
        assert!(status.success());
        super::update().await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            "Synthetic user state must survive Hermes updates."
        );
        assert!(
            python.exists(),
            "Previous dependency generation must remain recoverable"
        );
        assert!(!component.join("update-journal.json").exists());
        let python = super::managed_python(&component).unwrap();
        let status = tokio::process::Command::new(python)
            .args(["-c", "from hermes_state import SessionDB; db=SessionDB(); assert any(m['content']=='Synthetic update preservation message.' for m in db.get_messages('echo_update_fixture')); db.close()"])
            .current_dir(component.join("source"))
            .env("HERMES_HOME", component.join("home")).status().await.unwrap();
        assert!(status.success(), "Native messages must survive migration");
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD", "origin/main"])
            .current_dir(component.join("source"))
            .output()
            .unwrap();
        assert!(output.status.success());
        let revisions = String::from_utf8(output.stdout).unwrap();
        let mut lines = revisions.lines();
        assert_eq!(
            lines.next(),
            lines.next(),
            "Native update must select fetched main"
        );
    }
    #[test]
    fn native_google_parameters_are_validated_before_startup() {
        assert!(super::validate_google_parameters(
            &serde_json::json!({"generationConfig": {"thinkingConfig": {"thinkingLevel": "low"}}})
        )
        .is_ok());
        assert!(
            super::validate_google_parameters(&serde_json::json!({"unknown": true}))
                .unwrap_err()
                .contains("unknown")
        );
        assert!(
            super::validate_google_parameters(&serde_json::json!({"generationConfig": []}))
                .is_err()
        );
    }
    #[test]
    fn native_board_write_grant_is_removed_before_codex_parses_windows_overrides() {
        let args = [
            "app-server",
            "-c",
            r#"sandbox_workspace_write.writable_roots=["C:\Users\War\kanban"]"#,
            "-c",
            "mcp_servers.hermes-tools.env.HERMES_KANBAN_TASK=\"native-task-123\"",
            "-c",
            "sandbox_mode=\"workspace-write\"",
        ]
        .map(str::to_owned);
        let kept = super::proxy_arguments(&args);
        assert!(!kept
            .iter()
            .any(|arg| arg.starts_with("sandbox_workspace_write.writable_roots=")));
        assert_eq!(super::proxy_task(&kept).as_deref(), Some("native-task-123"));
        assert!(kept
            .iter()
            .any(|arg| arg == "sandbox_mode=\"workspace-write\""));
    }
    #[test]
    fn task_identity_survives_native_executor_environment_scrubbing() {
        let args = [
            "app-server",
            "-c",
            "mcp_servers.hermes-tools.env.HERMES_KANBAN_TASK=\"native-task-123\"",
        ]
        .map(str::to_owned);
        assert_eq!(super::proxy_task(&args).as_deref(), Some("native-task-123"));
        let invalid = [
            "app-server",
            "-c",
            "mcp_servers.hermes-tools.env.HERMES_KANBAN_TASK=\"../escape\"",
        ]
        .map(str::to_owned);
        assert!(super::proxy_task(&invalid).is_none());
    }
    #[test]
    fn browser_login_links_accept_only_official_https_hosts() {
        assert!(super::browser_login_url(
            "https://auth.openai.com/oauth/authorize?state=synthetic"
        )
        .is_some());
        for url in [
            "http://auth.openai.com/",
            "https://auth.openai.com.example.org/",
            "https://user:password@auth.openai.com/",
            "file:///auth.openai.com",
        ] {
            assert!(super::browser_login_url(url).is_none());
        }
    }
    #[tokio::test]
    #[ignore = "Requires the privately provisioned runtime in ECHO_HERMES_COMPONENT_DIR"]
    async fn managed_runtime_starts_and_stops_without_external_python() {
        assert!(
            std::env::var_os("ECHO_HERMES_COMPONENT_DIR").is_some(),
            "Use an isolated runtime directory"
        );
        let mut settings = crate::settings::AppSettings::default();
        let connection = crate::models::ModelConnection {
            id: "gate".into(),
            provider: "openai".into(),
            endpoint: "http://127.0.0.1:8999/v1".into(),
            api_key: "echo-test-placeholder".into(),
            api_version: String::new(),
            custom_header_name: String::new(),
            custom_header_value: String::new(),
            allow_lan_http: false,
        };
        settings.model_registry.connections.push(connection);
        let mut model =
            crate::models::ConfiguredModel::new("gate".into(), "Fixture".into(), "fixture".into());
        model.id = "gate".into();
        model.connection_id = "gate".into();
        model.model = "fixture".into();
        model.verified = true;
        settings.model_registry.models.push(model);
        settings.hermes.fast_model_id = "gate".into();
        settings.hermes.token = "e".repeat(64);
        settings.hermes.gateway_port = 18742;
        settings.hermes.bridge_port = 18743;
        let runtime = super::Runtime::start(&settings).await.unwrap();
        let client = crate::hermes::Client::new(&settings.hermes).unwrap();
        assert!(client.capabilities().await.is_ok());
        let session = client
            .create_session(&format!("Echo gate {}", crate::pipelines::new_id()))
            .await
            .unwrap();
        assert!(client.history_blocks(&session, None).await.is_ok());
        drop(runtime);
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(client.capabilities().await.is_err());
    }

    #[tokio::test]
    #[ignore = "Uses the selected assistant provider and an authenticated private Codex worker"]
    async fn authenticated_native_assistant_and_worker_readiness() {
        assert_eq!(std::env::var("ECHO_LIVE_HERMES_GATE").as_deref(), Ok("1"));
        assert!(std::env::var_os("ECHO_GATE_ECHO_BIN").is_some());
        let mut settings = crate::settings::load_settings();
        crate::hermes::inherit_assistant_model(&mut settings);
        assert!(
            !settings.hermes.fast_model_id.is_empty(),
            "The current assistant model is unavailable"
        );
        let direct = crate::models::complete(
            &settings,
            &settings.hermes.fast_model_id,
            "Readiness check. Do not use tools.",
            settings
                .model_registry
                .find(&settings.hermes.fast_model_id)
                .unwrap()
                .0
                .max_tokens,
            None,
            settings
                .model_registry
                .find(&settings.hermes.fast_model_id)
                .unwrap()
                .0
                .timeout_seconds,
            "Reply with exactly: Echo conversation ready.",
        )
        .await;
        println!("Existing Echo provider readiness: {}", direct.is_ok());
        if let Err(error) = &direct {
            // Classify without printing provider bodies, URLs or credentials.
            let lower = error.to_lowercase();
            let category = if lower.contains("429") || lower.contains("quota") {
                "rate limit or quota"
            } else if lower.contains("401") || lower.contains("403") {
                "authentication or access"
            } else if lower.contains("404") {
                "model or endpoint unavailable"
            } else if lower.contains("timeout") || lower.contains("timed out") {
                "request timeout"
            } else if lower.contains("400") {
                "request parameters rejected"
            } else if lower.contains("truncated") {
                "output token limit"
            } else if lower.contains("no text") {
                "empty model response"
            } else {
                "provider or transport error"
            };
            println!("Provider readiness failure category: {category}");
        }

        assert!(
            direct.is_ok(),
            "The currently configured Echo model rejected its own provider readiness request"
        );
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).unwrap();
        settings.hermes.token = bytes.iter().map(|b| format!("{b:02x}")).collect();
        settings.hermes.gateway_port = 18842;
        settings.hermes.bridge_port = 18843;
        let runtime = super::Runtime::start(&settings).await.unwrap();
        let client = crate::hermes::Client::new(&settings.hermes).unwrap();
        let session = client
            .create_session(&format!("Echo readiness {}", crate::pipelines::new_id()))
            .await
            .unwrap();
        let run = client.start_run(&session,"This is a readiness check. Do not use tools or inspect files. Reply with exactly: Echo conversation ready.",&crate::pipelines::new_id(),None).await.unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
        let events_client = client.clone();
        let events = tokio::spawn(async move { events_client.events(&run, sender).await });
        let complete = tokio::time::timeout(std::time::Duration::from_secs(90), async {
            let mut complete = false;
            while let Some(event) = receiver.recv().await {
                complete |= event.event == "run.completed";
            }
            complete
        })
        .await
        .unwrap();
        events.await.unwrap().unwrap();
        assert!(complete, "The current conversation model did not complete");
        println!("Current assistant model completed a native Hermes conversation.");
        let root = super::root();
        let python = super::managed_python(&root).unwrap();
        let mut command = tokio::process::Command::new(python);
        command.args(["-c","import json; from echo_bridge.plugin import _create; print(_create({'title':'Echo authenticated worker readiness','body':'Readiness check only. Do not inspect files, run commands, or modify anything. Call kanban_complete for this task with summary Echo worker ready.','intent':'inspect'}))"])
            .env("HERMES_HOME",root.join("home")).env("PYTHONPATH",root.join("bridge"))
            .stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let output = command.output().await.unwrap();
        assert!(output.status.success());
        let task: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let task_id = task["task_id"].as_str().unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(120), async {
            loop {
                let page = client.tasks().await.unwrap();
                if let Some(row) = page["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["id"] == task_id)
                {
                    match row["status"].as_str().unwrap_or_default() {
                        "done" => break Ok(()),
                        "blocked" | "archived" => break Err(row["status"].clone()),
                        _ => {}
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        })
        .await;
        let _ = client.stop_task(task_id).await;
        runtime.stop_tasks().await.unwrap();
        drop(runtime);
        assert!(
            matches!(result, Ok(Ok(()))),
            "Native worker readiness failed: {result:?}"
        );
        println!(
            "Authenticated Codex completed through the native Hermes dispatcher and Echo launcher."
        );
    }
}
