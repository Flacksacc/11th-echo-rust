//! Per-run tool registry and execution. Unknown MCP actions require approval.
use rig_core::completion::ToolDefinition;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub type Cancel = Arc<dyn Fn() -> bool + Send + Sync>;
#[derive(Clone, Copy, Default)]
pub struct Decision {
    pub allow: bool,
    pub scope: bool,
}
pub struct Approval {
    pub description: String,
    pub scope_label: String,
    pub reply: Mutex<Option<tokio::sync::oneshot::Sender<Decision>>>,
    pub cancelled: Cancel,
}
impl std::fmt::Debug for Approval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Approval { details withheld }")
    }
}
struct Remote {
    client: crate::assistant_mcp::Client,
    names: HashMap<String, String>,
    inspections: std::collections::HashSet<String>,
    output: Option<std::path::PathBuf>,
}
pub struct Runtime {
    pub definitions: Vec<ToolDefinition>,
    remotes: Vec<Remote>,
    settings: crate::settings::AppSettings,
    cancel: Cancel,
    observer: crate::graph::Observer,
    node: String,
    calls: Mutex<u32>,
    limit: u32,
    deadline: Instant,
    approval_time: Mutex<Duration>,
    timeout: u64,
    image_input: bool,
    scopes: Mutex<std::collections::HashSet<String>>,
}

pub fn builtin_tools() -> Vec<ToolDefinition> {
    let def = |name: &str, description: &str, properties: Value, required: Vec<&str>| {
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            parameters: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
        }
    };
    vec![
        def("system_info","Inspect Windows hardware, memory, disks, processes, services, or installed applications.",json!({"section":{"type":"string","enum":["hardware","memory","disks","processes","services","applications"]}}),vec!["section"]),
        def("list_files","List files in a directory without following links.",json!({"path":{"type":"string"}}),vec!["path"]),
        def("find_files","Find files by name below a directory; returns attachment IDs.",json!({"path":{"type":"string"},"query":{"type":"string"}}),vec!["path","query"]),
        def("read_file","Read a UTF-8 file and register it as an attachment.",json!({"path":{"type":"string"}}),vec!["path"]),
        def("attach_file","Register an existing file for a response attachment; returns an artifact_id.",json!({"path":{"type":"string"}}),vec!["path"]),
        def("write_file","Create or replace a UTF-8 file after approval.",json!({"path":{"type":"string"},"content":{"type":"string"}}),vec!["path","content"]),
        def("move_file","Move a file after approval; does not overwrite an existing destination.",json!({"path":{"type":"string"},"destination":{"type":"string"}}),vec!["path","destination"]),
        def("delete_file","Delete a single file after approval; never recursively deletes directories.",json!({"path":{"type":"string"}}),vec!["path"]),
        def("run_command","Run a PowerShell command after approval. Return stdout, stderr and exit status.",json!({"command":{"type":"string"}}),vec!["command"]),
        def("run_executable","Run an executable directly with an argument array after approval.",json!({"executable":{"type":"string"},"args":{"type":"array","items":{"type":"string"}}}),vec!["executable","args"]),
        def("web_search","Search the web using Brave and return titles, snippets and source URLs.",json!({"query":{"type":"string"}}),vec!["query"]),
    ]
}
pub fn identity(server: &str, tool: &str) -> String {
    format!("{server}::{tool}")
}
fn wire_name(server: &str, tool: &str) -> String {
    use sha2::Digest;
    let h = sha2::Sha256::digest(identity(server, tool).as_bytes());
    format!("mcp_{}", &format!("{h:x}")[..60])
}

impl Runtime {
    #[allow(clippy::too_many_arguments)]
    pub async fn connect(
        settings: &crate::settings::AppSettings,
        selected: &[String],
        image_input: bool,
        limit: u32,
        seconds: u64,
        timeout: u64,
        cancel: Cancel,
        observer: crate::graph::Observer,
        node: String,
    ) -> Result<Self, String> {
        let mut definitions: Vec<_> = builtin_tools()
            .into_iter()
            .filter(|t| selected.contains(&format!("builtin::{}", t.name)))
            .collect();
        let mut remotes = vec![];
        for server in settings.assistant_mcp_servers.iter().filter(|s| {
            s.enabled
                && selected
                    .iter()
                    .any(|id| id.starts_with(&format!("{}::", s.id)))
        }) {
            let client = crate::assistant_mcp::connect(server).await?;
            let tools =
                tokio::time::timeout(Duration::from_secs(15), client.peer().list_all_tools())
                    .await
                    .map_err(|_| "MCP discovery timed out.")?
                    .map_err(|_| "MCP discovery failed.")?;
            let mut names = HashMap::new();
            let trusted = ["browser", "desktop"].iter().any(|id| {
                crate::components::server(id).is_ok_and(|managed| {
                    managed.id == server.id
                        && managed.transport == server.transport
                        && managed.environment == server.environment
                        && managed.headers == server.headers
                })
            });
            let mut inspections = std::collections::HashSet::new();
            for tool in tools {
                if selected.contains(&identity(&server.id, &tool.name)) {
                    let name = wire_name(&server.id, &tool.name);
                    if trusted
                        && tool
                            .annotations
                            .as_ref()
                            .is_some_and(|a| a.read_only_hint == Some(true))
                    {
                        inspections.insert(name.clone());
                    }
                    definitions.push(ToolDefinition {
                        name: name.clone(),
                        description: format!(
                            "{}: {}",
                            server.name,
                            tool.description.as_deref().unwrap_or("MCP tool")
                        ),
                        parameters: Value::Object((*tool.input_schema).clone()),
                    });
                    names.insert(name, tool.name.to_string());
                }
            }
            remotes.push(Remote {
                client,
                names,
                inspections,
                output: (trusted && server.id == "echo-browser").then(crate::artifacts::root),
            });
        }
        if selected
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            > definitions.len()
        {
            return Err(
                "One or more selected tools are unavailable. Check Tools and MCP settings.".into(),
            );
        }
        Ok(Self {
            scopes: Mutex::new(Default::default()),
            image_input,
            definitions,
            remotes,
            settings: settings.clone(),
            cancel,
            observer,
            node,
            calls: Mutex::new(0),
            limit: limit.clamp(1, 128),
            deadline: Instant::now() + Duration::from_secs(seconds.clamp(30, 3600)),
            approval_time: Mutex::new(Duration::ZERO),
            timeout: timeout.clamp(5, 600),
        })
    }
    pub fn check(&self) -> Result<(), String> {
        if (self.cancel)() {
            Err("Agent interrupted.".into())
        } else if Instant::now() > self.deadline + *self.approval_time.lock().unwrap() {
            Err("Agent reached its execution time limit.".into())
        } else {
            Ok(())
        }
    }
    fn activity(&self, text: String) {
        (self.observer)(crate::graph::Event::Activity {
            node: self.node.clone(),
            text,
        });
    }
    pub async fn guarded<T>(
        &self,
        operation: impl std::future::Future<Output = T>,
    ) -> Result<T, String> {
        self.check()?;
        tokio::select! {
            value=operation=>Ok(value),
            error=async { loop { tokio::time::sleep(Duration::from_millis(30)).await; if let Err(error)=self.check(){return error;} } }=>Err(error)
        }
    }
    pub async fn execute(
        &self,
        name: &str,
        args: Value,
    ) -> Result<Vec<rig_core::message::ToolResultContent>, String> {
        self.check()?;
        let definition = self
            .definitions
            .iter()
            .find(|d| d.name == name)
            .ok_or("Model requested an unselected tool.")?;
        if args.to_string().len() > 1024 * 1024 {
            return Err("Tool arguments exceeded the size limit.".into());
        }
        validate_args(&definition.parameters, &args)?;
        {
            let mut calls = self.calls.lock().unwrap();
            if *calls >= self.limit {
                return Err("Agent reached its tool-call limit.".into());
            }
            *calls += 1;
        }
        let produces_file = args.as_object().is_some_and(|args| {
            args.iter().any(|(key, value)| {
                [
                    "filename",
                    "file_path",
                    "output_path",
                    "screenshot_out_file",
                ]
                .contains(&key.as_str())
                    && value.as_str().is_some_and(|v| !v.is_empty())
            })
        });
        let inspection = !produces_file
            && ([
                "system_info",
                "list_files",
                "find_files",
                "read_file",
                "attach_file",
                "web_search",
            ]
            .contains(&name)
                || self
                    .remotes
                    .iter()
                    .any(|remote| remote.inspections.contains(name)));
        let target = if let Some(path) = args.get("path") {
            json!({"path":path,"destination":args.get("destination")})
        } else if args.get("window_id").is_some() && args.get("pid").is_some() {
            json!({"pid":args["pid"],"window_id":args["window_id"]})
        } else {
            Value::Null
        };
        let scope = if target.is_null() {
            String::new()
        } else {
            format!("{name}: {target}")
        };
        if !inspection && (scope.is_empty() || !self.scopes.lock().unwrap().contains(&scope)) {
            self.activity(format!("Waiting for approval: {}", definition.description));
            let (tx, rx) = tokio::sync::oneshot::channel();
            let request = Arc::new(Approval {
                scope_label: scope.clone(),
                description: format!(
                    "{}\n\n{}",
                    definition.description,
                    serde_json::to_string_pretty(&args).map_err(|_| "Invalid tool arguments.")?
                ),
                reply: Mutex::new(Some(tx)),
                cancelled: self.cancel.clone(),
            });
            (self.observer)(crate::graph::Event::Approval {
                node: self.node.clone(),
                request,
            });
            let start = Instant::now();
            let accepted = tokio::select! {value=rx=>value.unwrap_or_default(),_=async{while !(self.cancel)(){tokio::time::sleep(Duration::from_millis(30)).await;}}=>return Err("Agent interrupted.".into())};
            // Active execution time excludes approval waiting.
            *self.approval_time.lock().unwrap() += start.elapsed();
            if accepted.allow && accepted.scope && !scope.is_empty() {
                self.scopes.lock().unwrap().insert(scope);
            }
            if !accepted.allow {
                self.activity(format!("Denied: {name}"));
                return Ok(vec![rig_core::message::ToolResultContent::text(
                    "User denied this action. Do not retry it without a new instruction.",
                )]);
            }
        }
        self.check()?;
        self.activity(format!("Running: {name}"));
        let operation = async {
            for remote in &self.remotes {
                if let Some(original) = remote.names.get(name) {
                    let params = rmcp::model::CallToolRequestParams::new(original.clone())
                        .with_arguments(args.as_object().cloned().unwrap_or_default());
                    let result = remote
                        .client
                        .peer()
                        .call_tool(params)
                        .await
                        .map_err(|_| "MCP tool failed.")?;
                    let mut result = result;
                    if let Some(output) = &remote.output {
                        for content in &mut result.content {
                            if let rmcp::model::ContentBlock::Text(text) = content {
                                text.text = inline_browser_output(&text.text, output)?;
                            }
                        }
                    }
                    return serde_json::to_string(&result)
                        .map_err(|_| "Invalid MCP tool result.".into());
                }
            }
            self.builtin(name, &args).await
        };
        let result = self
            .guarded(tokio::time::timeout(
                Duration::from_secs(self.timeout),
                operation,
            ))
            .await?
            .unwrap_or_else(|_| Err("Tool timed out.".into()));
        self.activity(format!(
            "{}: {name}",
            if result.is_ok() {
                "Completed"
            } else {
                "Failed"
            }
        ));
        let result = result.or_else(|error| {
            self.check()?;
            Ok::<_, String>(json!({"isError":true,"error":error}).to_string())
        });
        result.and_then(|mut text|{
            let mut images=vec![];
            if let Ok(mut result)=serde_json::from_str::<Value>(&text){
                if let Some(content)=result["content"].as_array_mut(){for item in content{
                    let resource=if item["type"]=="resource"{item.get_mut("resource").ok_or("Missing MCP resource.")?}else{item};
                    let data=resource["data"].as_str().or_else(||resource["blob"].as_str()).map(str::to_owned);
                    if let Some(data)=data{use base64::Engine;if data.len()>70*1024*1024{return Err("Tool attachment exceeded the size limit.".into());}let bytes=base64::engine::general_purpose::STANDARD.decode(&data).map_err(|_|"Invalid tool attachment encoding.")?;if bytes.len()>50*1024*1024{return Err("Tool attachment exceeded 50 MB.".into());}
                        let mime=resource["mimeType"].as_str().unwrap_or("application/octet-stream").to_owned();
                        if mime.starts_with("image/"){
                            let reader=image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().map_err(|_|"Invalid tool image.")?;
                            let expected=match mime.as_str(){"image/png"=>image::ImageFormat::Png,"image/jpeg"=>image::ImageFormat::Jpeg,"image/webp"=>image::ImageFormat::WebP,_=>return Err("Unsupported tool image type.".into())};
                            if reader.format()!=Some(expected){return Err("Tool image did not match its declared format.".into());}
                            let(width,height)=reader.into_dimensions().map_err(|_|"Invalid tool image.")?;if width==0||height==0||width>16384||height>16384||u64::from(width)*u64::from(height)>64*1024*1024{return Err("Tool image dimensions exceeded the limit.".into());}
                            if !self.image_input{return Err("This tool returned an image. Enable Image input on the Agent node and select a vision-capable model.".into());}
                            let media_type=match mime.as_str(){"image/png"=>rig_core::message::ImageMediaType::PNG,"image/jpeg"=>rig_core::message::ImageMediaType::JPEG,"image/webp"=>rig_core::message::ImageMediaType::WEBP,_=>return Err("Unsupported tool image type.".into())};
                            images.push(rig_core::message::ToolResultContent::Image(rig_core::message::Image{data:rig_core::message::DocumentSourceKind::Base64(data),media_type:Some(media_type),..Default::default()}));
                        }
                        let dir=crate::artifacts::root().join(crate::pipelines::new_id());std::fs::create_dir_all(&dir).map_err(|_|"Could not store tool attachment.")?;
                        let extension=match mime.as_str(){"image/png"=>"png","image/jpeg"=>"jpg","image/webp"=>"webp","application/pdf"=>"pdf",_=>"bin"};let path=dir.join(format!("attachment.{extension}"));std::fs::write(&path,&bytes).map_err(|_|"Could not store tool attachment.")?;
                        let artifact=crate::artifacts::register(&path,true,mime)?;*resource=json!({"artifact_id":artifact.id,"name":artifact.name});
                    }
                }}
                text=result.to_string();
            }
            if text.len()>65536{let mut end=65536;while !text.is_char_boundary(end){end-=1;}text.truncate(end);text.push_str("\n[Tool output truncated]");}
            let mut result=vec![rig_core::message::ToolResultContent::text(text)];result.extend(images);Ok(result)
        })
    }
    async fn builtin(&self, name: &str, args: &Value) -> Result<String, String> {
        let text = |key: &str| args[key].as_str().ok_or_else(|| format!("Missing {key}."));
        match name {
            "system_info" => {
                let script=match text("section")?{"hardware"=>"Get-CimInstance Win32_ComputerSystem | Select-Object Manufacturer,Model,TotalPhysicalMemory,NumberOfLogicalProcessors | ConvertTo-Json", "memory"=>"Get-CimInstance Win32_OperatingSystem | Select-Object TotalVisibleMemorySize,FreePhysicalMemory,TotalVirtualMemorySize,FreeVirtualMemory | ConvertTo-Json","disks"=>"Get-CimInstance Win32_LogicalDisk | Select-Object DeviceID,Size,FreeSpace | ConvertTo-Json","processes"=>"Get-Process | Select-Object Id,ProcessName,CPU,WorkingSet64 | ConvertTo-Json","services"=>"Get-Service | Select-Object Name,Status | ConvertTo-Json","applications"=>"Get-ItemProperty HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*,HKLM:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\* -ErrorAction SilentlyContinue | Select-Object DisplayName,DisplayVersion | ConvertTo-Json",_=>return Err("Unknown inspection section.".into())};
                command(
                    "powershell.exe",
                    &[
                        "-NoProfile".into(),
                        "-NonInteractive".into(),
                        "-Command".into(),
                        script.into(),
                    ],
                )
                .await
            }
            "run_command" => {
                command(
                    "powershell.exe",
                    &[
                        "-NoProfile".into(),
                        "-NonInteractive".into(),
                        "-Command".into(),
                        text("command")?.into(),
                    ],
                )
                .await
            }
            "run_executable" => {
                command(
                    text("executable")?,
                    &args["args"]
                        .as_array()
                        .ok_or("Arguments must be an array.")?
                        .iter()
                        .map(|a| {
                            a.as_str()
                                .map(str::to_owned)
                                .ok_or("Arguments must be strings.")
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                )
                .await
            }
            "web_search" => {
                if self.settings.brave_api_key.trim().is_empty() {
                    return Err("Set a Brave Search API key in Tools settings.".into());
                }
                let client = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|_| "Could not create search client.")?;
                let response = client
                    .get("https://api.search.brave.com/res/v1/web/search")
                    .header("X-Subscription-Token", &self.settings.brave_api_key)
                    .query(&[("q", text("query")?), ("count", "8")])
                    .send()
                    .await
                    .map_err(|_| "Web search failed.")?
                    .error_for_status()
                    .map_err(|_| "Brave Search rejected the request; check the API key.")?;
                let bytes = response
                    .bytes()
                    .await
                    .map_err(|_| "Invalid search response.")?;
                if bytes.len() > 1024 * 1024 {
                    return Err("Search response exceeded the limit.".into());
                }
                let value: Value =
                    serde_json::from_slice(&bytes).map_err(|_| "Invalid search response.")?;
                Ok(value["web"]["results"].to_string())
            }
            "read_file" => {
                let path = std::path::Path::new(text("path")?);
                let metadata = tokio::fs::metadata(path)
                    .await
                    .map_err(|_| "Could not read file metadata.")?;
                if metadata.len() > 1024 * 1024 {
                    return Err("File is too large to read as text; attach it instead.".into());
                }
                let content = tokio::fs::read_to_string(path)
                    .await
                    .map_err(|_| "Could not read UTF-8 file.")?;
                let artifact = crate::artifacts::register(path, false, "text/plain".into())?;
                Ok(json!({"artifact_id":artifact.id,"content":content}).to_string())
            }
            "attach_file" => {
                let artifact = crate::artifacts::register(
                    std::path::Path::new(text("path")?),
                    false,
                    String::new(),
                )?;
                Ok(serde_json::to_string(&artifact).unwrap())
            }
            "list_files" | "find_files" => {
                let path = std::path::PathBuf::from(text("path")?);
                let query = if name == "find_files" {
                    Some(text("query")?.to_lowercase())
                } else {
                    None
                };
                let cancel = self.cancel.clone();
                tokio::task::spawn_blocking(move || list_files(path, query, cancel))
                    .await
                    .map_err(|_| "File search failed.")?
            }
            "write_file" => {
                let content = text("content")?;
                if content.len() > 1024 * 1024 {
                    return Err("File content exceeded the limit.".into());
                }
                tokio::fs::write(text("path")?, content)
                    .await
                    .map_err(|_| "Could not write file.")?;
                Ok("File written.".into())
            }
            "move_file" => {
                let destination = text("destination")?;
                if std::path::Path::new(destination).exists() {
                    return Err("Destination already exists.".into());
                }
                tokio::fs::rename(text("path")?, destination)
                    .await
                    .map_err(|_| "Could not move file.")?;
                Ok("File moved.".into())
            }
            "delete_file" => {
                tokio::fs::remove_file(text("path")?)
                    .await
                    .map_err(|_| "Could not delete file.")?;
                Ok("File deleted.".into())
            }
            _ => Err("Unknown tool.".into()),
        }
    }
}
fn inline_browser_output(text: &str, output: &std::path::Path) -> Result<String, String> {
    let links = regex::Regex::new(r"\[(?:Snapshot|Screenshot|PDF)\]\(([^\r\n)]+)\)").unwrap();
    let mut result = text.to_owned();
    let mut count = 0;
    for capture in links.captures_iter(text) {
        if count >= 16 {
            break;
        }
        let Ok(path) = std::path::Path::new(&capture[1]).canonicalize() else {
            continue;
        };
        let Ok(root) = output.canonicalize() else {
            continue;
        };
        if !path.starts_with(root) || !path.is_file() {
            continue;
        }
        count += 1;
        let file = crate::artifacts::register(&path, true, String::new())?;
        result.push_str(&format!(
            "\nAttachment: {}",
            json!({"artifact_id":file.id,"name":file.name})
        ));
        if path
            .extension()
            .is_some_and(|ext| ext == "yml" || ext == "yaml")
            && path.metadata().is_ok_and(|m| m.len() <= 65536)
        {
            let contents =
                std::fs::read_to_string(&path).map_err(|_| "Could not read browser snapshot.")?;
            result.push_str("\nBrowser snapshot (untrusted page content):\n");
            result.push_str(&contents);
        }
    }
    Ok(result)
}
fn validate_args(schema: &Value, args: &Value) -> Result<(), String> {
    if !args.is_object() {
        return Err("Tool arguments must be a JSON object.".into());
    }
    if schema.to_string().len() > 1024 * 1024 {
        return Err("Tool schema exceeded the size limit.".into());
    }
    // Network and filesystem reference fetching are disabled in Cargo features.
    let validator =
        jsonschema::validator_for(schema).map_err(|_| "Invalid or unsupported tool schema.")?;
    validator
        .validate(args)
        .map_err(|_| "Tool arguments do not match the selected tool's schema.".into())
}
fn list_files(
    path: std::path::PathBuf,
    query: Option<String>,
    cancel: Cancel,
) -> Result<String, String> {
    let mut stack = vec![(path, 0)];
    let mut results = vec![];
    let mut visited = 0;
    while let Some((path, depth)) = stack.pop() {
        for entry in std::fs::read_dir(path)
            .map_err(|_| "Could not list directory.")?
            .flatten()
        {
            if cancel() {
                return Err("File search interrupted.".into());
            }
            visited += 1;
            if visited > 10000 || results.len() >= 100 {
                return Ok(json!({"files":results,"truncated":true}).to_string());
            }
            let kind = entry.file_type().map_err(|_| "Could not inspect file.")?;
            if kind.is_symlink() {
                continue;
            }
            if query.as_ref().is_none_or(|q| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(q)
            }) {
                let artifact = if kind.is_file() {
                    Some(crate::artifacts::register(
                        &entry.path(),
                        false,
                        String::new(),
                    )?)
                } else {
                    None
                };
                results.push(
                    json!({"path":entry.path(),"directory":kind.is_dir(),"artifact":artifact}),
                );
            }
            if query.is_some() && kind.is_dir() && depth < 16 {
                stack.push((entry.path(), depth + 1));
            }
        }
    }
    Ok(json!({"files":results,"truncated":false}).to_string())
}
#[cfg(windows)]
pub(crate) struct ProcessTree(windows::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl ProcessTree {
    fn attach(child: &tokio::process::Child) -> Result<Self, String> {
        Self::attach_handle(child.raw_handle().unwrap_or(std::ptr::null_mut()))
    }
    pub(crate) fn attach_pid(pid: u32) -> Result<Self, String> {
        use windows::Win32::{
            Foundation::CloseHandle,
            System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
        };
        let handle = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid) }
            .map_err(|_| "Could not isolate MCP process.")?;
        let result = Self::attach_handle(handle.0 as std::os::windows::io::RawHandle);
        let _ = unsafe { CloseHandle(handle) };
        result
    }
    pub(crate) fn attach_handle(handle: std::os::windows::io::RawHandle) -> Result<Self, String> {
        use windows::Win32::{Foundation::HANDLE, System::JobObjects::*};
        let job = Self(
            unsafe { CreateJobObjectW(None, windows::core::PCWSTR::null()) }
                .map_err(|_| "Could not create command process group.")?,
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
            .and_then(|_| AssignProcessToJobObject(job.0, HANDLE(handle as isize)))
        }
        .map_err(|_| "Could not isolate command lifetime.")?;
        Ok(job)
    }
}
#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
    }
}
async fn command(executable: &str, args: &[String]) -> Result<String, String> {
    let mut process = tokio::process::Command::new(executable);
    process
        .args(args)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    process.creation_flags(0x08000000);
    let mut child = process.spawn().map_err(|_| "Could not launch command.")?;
    #[cfg(windows)]
    let guard = ProcessTree::attach(&child)?;
    let stdout = child.stdout.take().ok_or("Missing command output.")?;
    let stderr = child.stderr.take().ok_or("Missing command errors.")?;
    async fn read<R: tokio::io::AsyncRead + Unpin>(mut input: R) -> Result<String, String> {
        use tokio::io::AsyncReadExt;
        let mut data = vec![];
        let mut buffer = [0u8; 4096];
        loop {
            let n = input
                .read(&mut buffer)
                .await
                .map_err(|_| "Could not read command output.")?;
            if n == 0 {
                break;
            }
            let left = 65536usize.saturating_sub(data.len());
            data.extend_from_slice(&buffer[..n.min(left)]);
        }
        Ok(String::from_utf8_lossy(&data).into())
    }
    let (out, err, status) = tokio::join!(read(stdout), read(stderr), child.wait());
    // Kill any descendants still running after the parent exits as well.
    #[cfg(windows)]
    drop(guard);
    Ok(json!({"stdout":out?,"stderr":err?,"exit_code":status.map_err(|_|"Could not wait for command.")?.code()}).to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builtin_argument_validation() {
        let tools = builtin_tools();
        let tool = tools.iter().find(|t| t.name == "system_info").unwrap();
        assert!(validate_args(&tool.parameters, &json!({"section":"hardware"})).is_ok());
        assert!(validate_args(&tool.parameters, &json!({"section":"run anything"})).is_err());
        assert!(validate_args(&tool.parameters, &json!({})).is_err());
    }
    #[test]
    fn server_names_cannot_collide() {
        assert_ne!(wire_name("one", "read"), wire_name("two", "read"));
        assert_eq!(wire_name("one", "read").len(), 64);
    }
    #[test]
    fn nested_schema_and_local_references_are_validated_offline() {
        let schema = json!({"type":"object","properties":{"args":{"type":"array","items":{"$ref":"#/$defs/argument"}}},"$defs":{"argument":{"type":"string","minLength":1}},"required":["args"],"additionalProperties":false});
        assert!(validate_args(&schema, &json!({"args":["ok"]})).is_ok());
        assert!(validate_args(&schema, &json!({"args":[1]})).is_err());
        assert!(validate_args(&schema, &json!({"args":[""]})).is_err());
        assert!(validate_args(&json!({"$ref":"file:///C:/private.json"}), &json!({})).is_err());
        assert!(validate_args(&json!({"$ref":"https://example.com/schema"}), &json!({})).is_err());
    }
    #[test]
    fn browser_snapshot_links_are_confined_to_managed_output() {
        let dir = std::env::temp_dir().join(format!(
            "echo-browser-output-{}",
            crate::pipelines::new_id()
        ));
        std::fs::create_dir(&dir).unwrap();
        let root = dir.join("outputs");
        std::fs::create_dir(&root).unwrap();
        let inside = root.join("page.yml");
        let outside = dir.join("private.yml");
        std::fs::write(&inside, "- heading: synthetic page").unwrap();
        std::fs::write(&outside, "must not be read").unwrap();
        let result = inline_browser_output(
            &format!(
                "[Snapshot]({})\n[Snapshot]({})",
                inside.display(),
                outside.display()
            ),
            &root,
        )
        .unwrap();
        assert!(result.contains("synthetic page"));
        assert!(result.contains("artifact_id"));
        assert!(!result.contains("must not be read"));
        std::fs::remove_file(inside).unwrap();
        std::fs::remove_file(outside).unwrap();
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
