//! End-to-end provider wire tests using synthetic local HTTP responses.
use crate::{agent_tools, graph, models, settings::AppSettings};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
#[ignore = "Requires verified helper archives extracted under ECHO_TOOL_TEST_ROOT"]
async fn managed_mcp_helpers_discover_and_browser_navigates() {
    use crate::assistant_mcp::{McpServer, McpTransport};
    let root = std::path::PathBuf::from(std::env::var_os("ECHO_TOOL_TEST_ROOT").unwrap())
        .canonicalize()
        .unwrap();
    let config = |name: &str, command: std::path::PathBuf, args: Vec<String>| McpServer {
        id: format!("test-{name}"),
        name: name.into(),
        enabled: true,
        discovered_tools: vec![],
        environment: Default::default(),
        headers: Default::default(),
        transport: McpTransport::Stdio {
            command: command.to_string_lossy().into(),
            args,
        },
    };
    let desktop = config(
        "desktop",
        root.join("desktop/cua-driver.exe"),
        vec!["mcp".into(), "--direct".into()],
    );
    let client = crate::assistant_mcp::connect(&desktop).await.unwrap();
    let tools = client.peer().list_all_tools().await.unwrap();
    assert!(!tools.is_empty());
    std::fs::write(
        root.join("desktop-tools.json"),
        serde_json::to_vec_pretty(&tools).unwrap(),
    )
    .unwrap();
    client.cancel().await.unwrap();
    let browser = config(
        "browser",
        root.join("node/node-v24.21.0-win-x64/node.exe"),
        vec![
            root.join("browser/node_modules/@playwright/mcp/cli.js")
                .to_string_lossy()
                .into(),
            "--isolated".into(),
            "--headless".into(),
            "--browser".into(),
            "chrome".into(),
            "--executable-path".into(),
            root.join("chromium/chrome-win64/chrome.exe")
                .to_string_lossy()
                .into(),
            "--output-dir".into(),
            root.join("output").to_string_lossy().into(),
        ],
    );
    let client = crate::assistant_mcp::connect(&browser).await.unwrap();
    let tools = client.peer().list_all_tools().await.unwrap();
    assert!(tools.iter().any(|t| t.name == "browser_navigate"));
    std::fs::write(
        root.join("browser-tools.json"),
        serde_json::to_vec_pretty(&tools).unwrap(),
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 4096];
        assert!(socket.read(&mut request).await.unwrap() > 0);
        let html="<html><head><title>Echo helper test</title></head><body><h1>Browser integration works</h1></body></html>";
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",html.len()).as_bytes()).await.unwrap();
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        client.peer().call_tool(
            rmcp::model::CallToolRequestParams::new("browser_navigate").with_arguments(
                json!({"url":format!("http://127.0.0.1:{port}")})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    assert!(
        serde_json::to_string(&result)
            .unwrap()
            .contains("Echo helper test"),
        "{result:?}"
    );
    assert!(std::fs::read_dir(root.join("output"))
        .unwrap()
        .flatten()
        .any(
            |entry| entry.path().extension().is_some_and(|ext| ext == "yml")
                && std::fs::read_to_string(entry.path())
                    .is_ok_and(|text| text.contains("Browser integration works"))
        ));
    server.await.unwrap();
    client
        .peer()
        .call_tool(rmcp::model::CallToolRequestParams::new("browser_close"))
        .await
        .unwrap();
    client.cancel().await.unwrap();
}

async fn body(socket: &mut tokio::net::TcpStream) -> Value {
    let mut bytes = vec![];
    let mut buffer = [0u8; 4096];
    loop {
        let n = socket.read(&mut buffer).await.unwrap();
        assert_ne!(n, 0);
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
            }
        }
    }
}
async fn respond(socket: &mut tokio::net::TcpStream, delta: Value, finish: &str) {
    let chunk = json!({"id":"mock","object":"chat.completion.chunk","created":0,"model":"test","choices":[{"index":0,"delta":delta,"finish_reason":null}]});
    let terminal = json!({"id":"mock","object":"chat.completion.chunk","created":0,"model":"test","choices":[{"index":0,"delta":{},"finish_reason":finish}]});
    let response = format!("data: {chunk}\n\ndata: {terminal}\n\ndata: [DONE]\n\n");
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
}
fn settings(port: u16) -> AppSettings {
    let mut settings = AppSettings::default();
    settings
        .model_registry
        .connections
        .push(models::ModelConnection {
            id: "c".into(),
            provider: "openai_compatible".into(),
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            api_version: String::new(),
            custom_header_name: String::new(),
            custom_header_value: String::new(),
            api_key: String::new(),
            allow_lan_http: false,
        });
    let mut model = models::ConfiguredModel::new("c".into(), "Test".into(), "test".into());
    model.id = "test-model".into();
    model.verified = true;
    settings.model_registry.models.push(model);
    settings
}
const ANSWER:&str="<echo_assistant>\nHere is the result.\n</echo_assistant>\n<echo_data>\nComplete written result.\n</echo_data>\n<echo_files>\n[]\n</echo_files>\n";

#[tokio::test]
async fn tool_result_is_replayed_before_final_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let directory = std::env::temp_dir().join(format!("echo-agent-{}", crate::pipelines::new_id()));
    std::fs::create_dir(&directory).unwrap();
    let args = json!({"path":directory}).to_string();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = body(&mut socket).await;
        assert_eq!(request["tools"][0]["function"]["name"], "list_files");
        respond(&mut socket,json!({"tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"list_files","arguments":args}}]}),"tool_calls").await;
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = body(&mut socket).await;
        let messages = request["messages"].as_array().unwrap();
        assert_eq!(messages.last().unwrap()["role"], "tool");
        assert_eq!(messages.last().unwrap()["tool_call_id"], "call-1");
        assert!(messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("files"));
        respond(&mut socket, json!({"content":ANSWER}), "stop").await;
    });
    let settings = settings(port);
    let events = Arc::new(Mutex::new(vec![]));
    let captured = events.clone();
    let agent = agent_tools::Runtime::connect(
        &settings,
        &["builtin::list_files".into()],
        false,
        32,
        600,
        60,
        Arc::new(|| false),
        Arc::new(move |event| captured.lock().unwrap().push(event)),
        "node".into(),
    )
    .await
    .unwrap();
    let mut output = vec![];
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        models::stream_agent_component(
            &settings,
            "test-model",
            crate::response_channels::INSTRUCTIONS,
            2048,
            None,
            5,
            "Inspect the synthetic folder.",
            Some(&agent),
            |text| output.push(text.to_owned()),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result, ANSWER);
    assert_eq!(output.last().unwrap(), ANSWER);
    server.await.unwrap();
    std::fs::remove_dir(directory).unwrap();
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e,graph::Event::Activity{text,..} if text.starts_with("Completed:"))));
}

#[tokio::test]
async fn graph_separates_assistant_and_written_text() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        body(&mut socket).await;
        respond(&mut socket, json!({"content":ANSWER}), "stop").await;
    });
    let settings = settings(port);
    use crate::pipelines::{Step, StepKind};
    let source = graph::Node::new(
        graph::Kind::Component(Step::new(StepKind::Transcription {
            provider: "local_sherpa_onnx".into(),
            model: "parakeet-tdt-0.6b-v2-int8".into(),
            language: "en".into(),
            no_verbatim: false,
            local: Default::default(),
        })),
        0.,
        0.,
    );
    let llm = graph::Node::new(
        graph::Kind::Llm {
            step: Step::new(StepKind::Model {
                model_id: "test-model".into(),
                name: "Model".into(),
                instructions: String::new(),
                max_tokens: 2048,
                temperature: None,
                timeout_seconds: 5,
            }),
            history: false,
            tools: vec![],
            assistant_response: true,
        },
        300.,
        0.,
    );
    let display = graph::Node::new(graph::Kind::AssistantDisplay, 600., 0.);
    let mut graph = graph::Graph {
        nodes: vec![source.clone(), llm.clone(), display.clone()],
        edges: vec![],
        hotkey: String::new(),
        view: Default::default(),
        legacy_formatting: None,
    };
    graph
        .connect(&source.id, "text", &llm.id, "prompt")
        .unwrap();
    graph.connect(&llm.id, "text", &display.id, "text").unwrap();
    graph
        .connect(&llm.id, "assistant", &display.id, "assistant")
        .unwrap();
    graph
        .connect(&llm.id, "files", &display.id, "files")
        .unwrap();
    let events = Arc::new(Mutex::new(vec![]));
    let captured = events.clone();
    let result = graph::execute(
        &graph,
        "Question".into(),
        &settings,
        String::new(),
        Arc::new(move |e| captured.lock().unwrap().push(e)),
        Arc::new(|| false),
    )
    .await
    .unwrap();
    assert_eq!(result.text, "Complete written result.");
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e,graph::Event::Assistant{text,..}if text=="Here is the result.")));
    server.await.unwrap();
}

#[tokio::test]
async fn tool_after_final_response_is_never_executed() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        body(&mut socket).await;
        respond(&mut socket,json!({"content":"<echo_assistant>\nHello.\n","tool_calls":[{"index":0,"id":"late","type":"function","function":{"name":"delete_file","arguments":"{\"path\":\"unused\"}"}}]}),"tool_calls").await;
    });
    let settings = settings(port);
    let agent = agent_tools::Runtime::connect(
        &settings,
        &["builtin::delete_file".into()],
        false,
        32,
        600,
        60,
        Arc::new(|| false),
        Arc::new(|_| panic!("Late tool must never execute or ask approval")),
        "node".into(),
    )
    .await
    .unwrap();
    let result = models::stream_agent_component(
        &settings,
        "test-model",
        "",
        2048,
        None,
        5,
        "Test",
        Some(&agent),
        |_| {},
    )
    .await;
    assert!(result.unwrap_err().contains("after starting"));
    server.await.unwrap();
}

#[tokio::test]
async fn denied_changes_are_not_executed_and_scopes_end_with_the_run() {
    let settings = AppSettings::default();
    let requests = Arc::new(Mutex::new(0));
    let count = requests.clone();
    let observer = Arc::new(move |event| {
        if let graph::Event::Approval { request, .. } = event {
            *count.lock().unwrap() += 1;
            request
                .reply
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(agent_tools::Decision::default())
                .unwrap_or_else(|_| panic!("Receiver disappeared"));
        }
    });
    let runtime = agent_tools::Runtime::connect(
        &settings,
        &["builtin::write_file".into()],
        false,
        3,
        600,
        60,
        Arc::new(|| false),
        observer,
        "agent".into(),
    )
    .await
    .unwrap();
    let path = std::env::temp_dir().join(format!("echo-denied-{}.txt", crate::pipelines::new_id()));
    for _ in 0..2 {
        runtime
            .execute(
                "write_file",
                json!({"path":path,"content":"Must not be written"}),
            )
            .await
            .unwrap();
    }
    assert!(!path.exists());
    assert_eq!(*requests.lock().unwrap(), 2);
    assert!(runtime
        .execute("unselected", json!({}))
        .await
        .unwrap_err()
        .contains("unselected"));
}

#[tokio::test]
async fn interruption_cancels_a_pending_approval() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let cancelled = Arc::new(AtomicBool::new(false));
    let state = cancelled.clone();
    let pending = Arc::new(Mutex::new(None));
    let captured = pending.clone();
    let runtime = agent_tools::Runtime::connect(
        &AppSettings::default(),
        &["builtin::delete_file".into()],
        false,
        3,
        600,
        60,
        Arc::new(move || state.load(Ordering::SeqCst)),
        Arc::new(move |event| {
            if let graph::Event::Approval { request, .. } = event {
                *captured.lock().unwrap() = Some(request);
            }
        }),
        "agent".into(),
    )
    .await
    .unwrap();
    let request = runtime.execute("delete_file", json!({"path":"unused"}));
    let interrupt = async {
        while pending.lock().unwrap().is_none() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        cancelled.store(true, Ordering::SeqCst);
    };
    let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(request, interrupt)
    })
    .await
    .unwrap();
    assert!(result.unwrap_err().contains("interrupted"));
    let approval = pending.lock().unwrap().take().unwrap();
    assert!((approval.cancelled)());
    assert!(approval
        .reply
        .lock()
        .unwrap()
        .take()
        .unwrap()
        .send(agent_tools::Decision {
            allow: true,
            scope: true
        })
        .is_err());
}

#[tokio::test]
async fn scoped_approval_applies_only_to_the_same_file_for_one_runtime() {
    let directory = std::env::temp_dir().join(format!("echo-scope-{}", crate::pipelines::new_id()));
    std::fs::create_dir(&directory).unwrap();
    let requests = Arc::new(Mutex::new(0));
    let count = requests.clone();
    let observer: graph::Observer = Arc::new(move |event| {
        if let graph::Event::Approval { request, .. } = event {
            *count.lock().unwrap() += 1;
            let _ = request
                .reply
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(agent_tools::Decision {
                    allow: true,
                    scope: true,
                });
        }
    });
    let settings = AppSettings::default();
    let tools = ["builtin::write_file".into()];
    let runtime = agent_tools::Runtime::connect(
        &settings,
        &tools,
        false,
        5,
        600,
        60,
        Arc::new(|| false),
        observer.clone(),
        "agent".into(),
    )
    .await
    .unwrap();
    for name in ["one.txt", "one.txt", "two.txt"] {
        runtime
            .execute(
                "write_file",
                json!({"path":directory.join(name),"content":"synthetic"}),
            )
            .await
            .unwrap();
    }
    assert_eq!(*requests.lock().unwrap(), 2);
    let next = agent_tools::Runtime::connect(
        &settings,
        &tools,
        false,
        5,
        600,
        60,
        Arc::new(|| false),
        observer,
        "agent".into(),
    )
    .await
    .unwrap();
    next.execute(
        "write_file",
        json!({"path":directory.join("one.txt"),"content":"next turn"}),
    )
    .await
    .unwrap();
    assert_eq!(*requests.lock().unwrap(), 3);
    std::fs::remove_file(directory.join("one.txt")).unwrap();
    std::fs::remove_file(directory.join("two.txt")).unwrap();
    std::fs::remove_dir(directory).unwrap();
}
