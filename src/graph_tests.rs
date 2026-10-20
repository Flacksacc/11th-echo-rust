use super::*;
use crate::pipelines::{migrate as legacy_migrate, AssistantConfig};
use slint::Model;

fn fixture() -> (AppSettings, Graph) {
    let mut s = AppSettings::default();
    s.post_processing.enabled = false;
    let p = legacy_migrate(&s).active().unwrap().clone();
    let g = migrate(&p, &s);
    (s, g)
}

#[tokio::test]
async fn hermes_prepares_templates_and_wired_instructions_without_running_response_or_speech() {
    let (settings, mut graph) = fixture();
    let source = graph
        .nodes
        .iter()
        .find(|node| node.transcription())
        .unwrap()
        .id
        .clone();
    graph.nodes.retain(|node| node.transcription());
    graph.edges.clear();
    let template = add(
        &mut graph,
        Kind::Template {
            template: "Request: {text}".into(),
        },
    );
    wire(&mut graph, &source, "text", &template, "text");
    let response = add(
        &mut graph,
        Kind::Llm {
            step: Step::new(StepKind::Model {
                model_id: "uninstalled-response-model".into(),
                name: "Assistant".into(),
                instructions: "old instructions".into(),
                max_tokens: 512,
                temperature: None,
                timeout_seconds: 30,
            }),
            history: true,
            tools: vec![],
            assistant_response: false,
        },
    );
    wire(&mut graph, &template, "text", &response, "prompt");
    let instructions = add(
        &mut graph,
        Kind::Text {
            text: "Use concise answers.".into(),
        },
    );
    wire(&mut graph, &instructions, "text", &response, "instructions");
    let display = add(&mut graph, Kind::AssistantDisplay);
    wire(&mut graph, &response, "text", &display, "text");
    let speech = add(
        &mut graph,
        Kind::Speech {
            voice: "missing-voice".into(),
            preload: false,
        },
    );
    wire(&mut graph, &response, "text", &speech, "text");
    let result = prepare_hermes(
        &graph,
        "hello world".into(),
        &settings,
        Arc::new(|_| {}),
        Arc::new(|| false),
    )
    .await
    .unwrap();
    assert_eq!(result.text, "Request: hello world");
    assert_eq!(result.instructions, "Use concise answers.");
    assert!(result.errors.is_empty());
    assert_eq!(result.speech_voice.as_deref(), Some("missing-voice"));
    // Saving a changed Speech node must change the next native response,
    // without running a model or accessing audio hardware during preparation.
    if let Kind::Speech { voice, .. } = &mut graph
        .nodes
        .iter_mut()
        .find(|node| node.id == speech)
        .unwrap()
        .kind
    {
        *voice = "replacement-voice.safetensors".into();
    }
    let saved: Graph = serde_json::from_str(&serde_json::to_string(&graph).unwrap()).unwrap();
    let updated = prepare_hermes(
        &saved,
        "next turn".into(),
        &settings,
        Arc::new(|_| {}),
        Arc::new(|| false),
    )
    .await
    .unwrap();
    assert_eq!(
        updated.speech_voice.as_deref(),
        Some("replacement-voice.safetensors")
    );
    let conflicting = add(
        &mut graph,
        Kind::Speech {
            voice: "another-voice.safetensors".into(),
            preload: false,
        },
    );
    wire(&mut graph, &response, "text", &conflicting, "text");
    assert!(prepare_hermes(
        &graph,
        "ambiguous".into(),
        &settings,
        Arc::new(|_| {}),
        Arc::new(|| false)
    )
    .await
    .unwrap_err()
    .contains("different Speech voices"));
    graph.nodes.retain(|node| node.id != conflicting);
    graph.edges.retain(|edge| edge.to != conflicting);
    graph.nodes.retain(|node| node.id != speech);
    graph.edges.retain(|edge| edge.to != speech);
    let unrelated = add(
        &mut graph,
        Kind::Speech {
            voice: "unrelated-input-voice.safetensors".into(),
            preload: false,
        },
    );
    wire(&mut graph, &source, "text", &unrelated, "text");
    let silent = prepare_hermes(
        &graph,
        "text only".into(),
        &settings,
        Arc::new(|_| {}),
        Arc::new(|| false),
    )
    .await
    .unwrap();
    assert_eq!(silent.speech_voice, None);
}

#[tokio::test]
async fn native_assistant_uses_only_the_routed_response_voice_after_save() {
    let (mut settings, mut graph, router, options) = routed();
    for (index, option) in options.iter().enumerate().skip(1) {
        let edge = graph
            .edges
            .iter()
            .find(|edge| edge.from == router && edge.output == *option)
            .unwrap()
            .clone();
        graph.edges.retain(|candidate| candidate != &edge);
        let response = add(
            &mut graph,
            Kind::Llm {
                step: Step::new(StepKind::Model {
                    model_id: "response-not-run".into(),
                    name: "Assistant".into(),
                    instructions: format!("branch {index}"),
                    max_tokens: 512,
                    temperature: None,
                    timeout_seconds: 30,
                }),
                history: true,
                tools: vec![],
                assistant_response: false,
            },
        );
        wire(&mut graph, &router, option, &response, "prompt");
        wire(&mut graph, &response, "text", &edge.to, "text");
        let speech = add(
            &mut graph,
            Kind::Speech {
                voice: format!("saved-branch-{index}.safetensors"),
                preload: false,
            },
        );
        wire(&mut graph, &response, "text", &speech, "text");
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    settings
        .model_registry
        .connections
        .push(crate::models::ModelConnection {
            id: "test".into(),
            provider: "openai_compatible".into(),
            endpoint: format!("http://{address}/v1"),
            api_key: String::new(),
            api_version: String::new(),
            custom_header_name: String::new(),
            custom_header_value: String::new(),
            allow_lan_http: false,
        });
    let mut model =
        crate::models::ConfiguredModel::new("test".into(), "Router".into(), "test".into());
    model.id = "router".into();
    model.verified = true;
    settings.model_registry.models.push(model);
    let choice = options[2].clone();
    let server = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = vec![0; 16384];
        socket.read(&mut buffer).await.unwrap();
        let content = serde_json::json!({"choice": choice}).to_string();
        let body = serde_json::json!({"id":"test","object":"chat.completion","created":0,"model":"test","choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    graph.nodes.reverse();
    let graph: Graph = serde_json::from_str(&serde_json::to_string(&graph).unwrap()).unwrap();
    let prepared = prepare_hermes(
        &graph,
        "request".into(),
        &settings,
        Arc::new(|_| {}),
        Arc::new(|| false),
    )
    .await
    .unwrap();
    server.await.unwrap();
    assert!(prepared.errors.is_empty(), "{:?}", prepared.errors);
    assert_eq!(
        prepared.speech_voice.as_deref(),
        Some("saved-branch-2.safetensors")
    );
    assert_eq!(prepared.instructions, "branch 2");
}
fn add(g: &mut Graph, kind: Kind) -> String {
    let n = Node::new(kind, 300., 300.);
    let id = n.id.clone();
    g.nodes.push(n);
    id
}
fn routed() -> (AppSettings, Graph, String, Vec<String>) {
    let (s, mut g) = fixture();
    let source = g
        .nodes
        .iter()
        .find(|n| n.transcription())
        .unwrap()
        .id
        .clone();
    g.nodes.retain(|n| n.transcription());
    g.edges.clear();
    let options = vec![new_id(), new_id(), new_id()];
    let router = add(
        &mut g,
        Kind::Router {
            model_id: "router".into(),
            options: options.clone(),
            fallback: None,
            threshold: 0.,
            preload: false,
        },
    );
    wire(&mut g, &source, "text", &router, "prompt");
    for (i, id) in options.iter().enumerate() {
        let text = add(
            &mut g,
            Kind::Text {
                text: format!("Option {i}"),
            },
        );
        wire(&mut g, &text, "text", &router, id);
        let end = add(
            &mut g,
            if i == 0 {
                Kind::TextOutput
            } else {
                Kind::AssistantDisplay
            },
        );
        wire(&mut g, &router, id, &end, "text");
    }
    (s, g, router, options)
}

#[test]
fn graph_connections_are_typed_single_input_and_acyclic() {
    let (_, mut g) = fixture();
    g.validate().unwrap();
    let e = g.edges[0].clone();
    assert!(g
        .connect(&e.from, &e.output, &e.to, &e.input)
        .unwrap_err()
        .contains("one wire"));
    let a = add(
        &mut g,
        Kind::Combine {
            inputs: vec!["a".into()],
            separator: "".into(),
        },
    );
    let b = add(
        &mut g,
        Kind::Combine {
            inputs: vec!["b".into()],
            separator: "".into(),
        },
    );
    g.connect(&a, "text", &b, "b").unwrap();
    assert!(g.connect(&b, "text", &a, "a").is_err());
    assert!(g.connect(&a, "audio", &b, "b").is_err());
}
#[test]
fn graph_router_choices_have_exclusive_covered_destinations() {
    let (_, mut g, router, options) = routed();
    g.validate().unwrap();
    let extra = add(&mut g, Kind::TextOutput);
    wire(&mut g, &router, &options[0], &extra, "text");
    assert!(g.validate().unwrap_err().contains("Two ending"));
    g.remove(&extra);
    let index = g
        .edges
        .iter()
        .position(|e| e.from == router && e.output == options[2])
        .unwrap();
    g.disconnect(index);
    assert!(g.validate().unwrap_err().contains("Every router choice"));
}
#[test]
fn graph_router_grows_options_and_removes_paired_output() {
    let (_, mut g, router, _) = routed();
    let t = add(
        &mut g,
        Kind::Text {
            text: "Fourth".into(),
        },
    );
    g.connect(&t, "text", &router, "new_option").unwrap();
    let n = g.nodes.iter().find(|n| n.id == router).unwrap();
    assert_eq!(n.outputs().len(), 4);
    assert_eq!(n.inputs().last().unwrap().id, "new_option");
    let id = n.outputs()[3].id.clone();
    let end = add(&mut g, Kind::TextOutput);
    g.connect(&router, &id, &end, "text").unwrap();
    let index = g
        .edges
        .iter()
        .position(|e| e.to == router && e.input == id)
        .unwrap();
    g.disconnect(index);
    assert!(!g.edges.iter().any(|e| e.from == router && e.output == id));
    assert_eq!(
        g.nodes
            .iter()
            .find(|n| n.id == router)
            .unwrap()
            .outputs()
            .len(),
        3
    );
}
#[test]
fn graph_migration_roundtrip_preserves_preload_and_hotkeys() {
    let (mut s, _) = fixture();
    s.pipelines = Some(legacy_migrate(&s));
    let preload = s.pipelines.as_ref().unwrap().active().unwrap().steps[0].preload_on_start;
    let id = s.pipelines.as_ref().unwrap().active_id.clone();
    crate::graph_ui::upgrade(&mut s);
    let set = s.pipelines.as_ref().unwrap();
    assert_eq!(set.version, 5);
    let p = set.active().unwrap();
    assert_eq!(p.id, id);
    assert_eq!(p.graph.as_ref().unwrap().hotkey, s.hotkey_text);
    assert_eq!(
        p.graph
            .as_ref()
            .unwrap()
            .transcription()
            .unwrap()
            .preload_on_start,
        preload
    );
    let first = s.pipelines.clone();
    crate::graph_ui::upgrade(&mut s);
    assert_eq!(first, s.pipelines);
    let json = serde_json::to_string(&s.pipelines).unwrap();
    let reloaded: Option<crate::pipelines::PipelineSet> = serde_json::from_str(&json).unwrap();
    assert_eq!(reloaded, s.pipelines);
}
#[test]
fn graph_assistant_migration_has_separate_endings_and_models() {
    let (s, _) = fixture();
    let mut p = legacy_migrate(&s).active().unwrap().clone();
    p.assistant = Some(AssistantConfig {
        router_model_id: "fast".into(),
        answer_model_id: "strong".into(),
        instructions: "Be concise".into(),
        ..Default::default()
    });
    let g = migrate(&p, &s);
    g.validate().unwrap();
    assert_eq!(g.nodes.iter().filter(|n| n.ending()).count(), 2);
    assert_eq!(
        g.nodes
            .iter()
            .filter(|n| matches!(n.kind, Kind::Llm { .. }))
            .count(),
        2
    );
    assert!(g
        .nodes
        .iter()
        .any(|n| matches!(&n.kind,Kind::Text{text} if text=="Be concise")));
}
#[tokio::test]
async fn graph_strings_combine_prompt_with_user_without_replacement_recursion() {
    let (s, mut g) = fixture();
    g.nodes.retain(|n| n.transcription());
    g.edges.clear();
    let source = g.nodes[0].id.clone();
    let constant = add(
        &mut g,
        Kind::Text {
            text: "Static {message}".into(),
        },
    );
    let template = add(
        &mut g,
        Kind::Template {
            template: "{instructions}: {message}".into(),
        },
    );
    wire(&mut g, &constant, "text", &template, "instructions");
    wire(&mut g, &source, "text", &template, "message");
    let end = add(&mut g, Kind::TextOutput);
    wire(&mut g, &template, "text", &end, "text");
    let out = execute(
        &g,
        "User text".into(),
        &s,
        "".into(),
        Arc::new(|_| {}),
        Arc::new(|| false),
    )
    .await
    .unwrap();
    assert_eq!(out.text, "Static {message}: User text");
    assert_eq!(out.destination, Destination::Text);
}
#[tokio::test]
async fn graph_text_setting_wires_override_preserved_inline_defaults() {
    let (settings, mut graph) = fixture();
    graph.nodes.retain(|node| node.transcription());
    graph.edges.clear();
    let source = graph.nodes[0].id.clone();
    let constant = add(
        &mut graph,
        Kind::Text {
            text: "Answer".into(),
        },
    );
    let separator = add(&mut graph, Kind::Text { text: " / ".into() });
    let combine = add(
        &mut graph,
        Kind::Combine {
            inputs: vec!["first".into(), "second".into()],
            separator: ": ".into(),
        },
    );
    wire(&mut graph, &source, "text", &combine, "first");
    wire(&mut graph, &constant, "text", &combine, "second");
    wire(
        &mut graph,
        &separator,
        "text",
        &combine,
        "setting:/node_config/separator",
    );
    let end = add(&mut graph, Kind::TextOutput);
    wire(&mut graph, &combine, "text", &end, "text");
    let out = execute(
        &graph,
        "Question".into(),
        &settings,
        "".into(),
        Arc::new(|_| {}),
        Arc::new(|| false),
    )
    .await
    .unwrap();
    assert_eq!(out.text, "Question / Answer");
    let node = graph.nodes.iter().find(|node| node.id == combine).unwrap();
    assert!(crate::node_settings::rows_for(node, &graph)
        .iter()
        .any(|field| field.label == "Separator" && field.wired));
    graph
        .edges
        .retain(|edge| edge.input != "setting:/node_config/separator");
    let out = execute(
        &graph,
        "Question".into(),
        &settings,
        "".into(),
        Arc::new(|_| {}),
        Arc::new(|| false),
    )
    .await
    .unwrap();
    assert_eq!(out.text, "Question: Answer");
    let node = graph.nodes.iter().find(|node| node.id == combine).unwrap();
    assert!(crate::node_settings::rows_for(node, &graph)
        .iter()
        .any(|field| field.label == "Separator" && !field.wired));
}
#[tokio::test]
async fn graph_failed_side_node_does_not_block_text_output() {
    let (s, mut g) = fixture();
    let source = g
        .nodes
        .iter()
        .find(|n| n.transcription())
        .unwrap()
        .id
        .clone();
    let bad = add(
        &mut g,
        Kind::Component(Step::new(StepKind::Model {
            model_id: "missing".into(),
            name: "Missing".into(),
            instructions: "".into(),
            max_tokens: 16,
            temperature: None,
            timeout_seconds: 5,
        })),
    );
    wire(&mut g, &source, "text", &bad, "text");
    g.preflight(&s).unwrap();
    let out = execute(
        &g,
        "hello".into(),
        &s,
        "".into(),
        Arc::new(|_| {}),
        Arc::new(|| false),
    )
    .await
    .unwrap();
    assert_eq!(out.text, "hello");
    assert_eq!(out.errors.len(), 1);
}
#[tokio::test]
async fn graph_cancellation_prevents_outputs() {
    let (s, g) = fixture();
    assert!(execute(
        &g,
        "hello".into(),
        &s,
        "".into(),
        Arc::new(|_| {}),
        Arc::new(|| true)
    )
    .await
    .unwrap_err()
    .contains("cancelled"));
}
#[test]
fn graph_constants_cannot_replace_transcription_dependency() {
    let (_, mut g) = fixture();
    let end = g.nodes.iter().find(|n| n.ending()).unwrap().id.clone();
    g.edges.clear();
    let text = add(
        &mut g,
        Kind::Text {
            text: "hello".into(),
        },
    );
    wire(&mut g, &text, "text", &end, "text");
    assert!(g.validate().is_err());
}

#[tokio::test]
async fn graph_routes_third_option_and_marks_other_paths_skipped() {
    let (mut s, mut g, router, options) = routed();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let choice = options[2].clone();
    s.model_registry
        .connections
        .push(crate::models::ModelConnection {
            id: "test".into(),
            provider: "openai_compatible".into(),
            endpoint: format!("http://{address}/v1"),
            api_key: "".into(),
            api_version: "".into(),
            custom_header_name: "".into(),
            custom_header_value: "".into(),
            allow_lan_http: false,
        });
    let mut model =
        crate::models::ConfiguredModel::new("test".into(), "Router".into(), "test".into());
    model.id = "router".into();
    model.verified = true;
    s.model_registry.models.push(model);
    let server = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = vec![0; 16384];
        let _ = socket.read(&mut buffer).await.unwrap();
        let content = serde_json::json!({"choice":choice}).to_string();
        let body=serde_json::json!({"id":"test","object":"chat.completion","created":0,"model":"test","choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    });
    // Reverse storage order exercises skip propagation independent of node layout/order.
    g.nodes.reverse();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = events.clone();
    let out = execute(
        &g,
        "request".into(),
        &s,
        "".into(),
        Arc::new(move |e| captured.lock().unwrap().push(e)),
        Arc::new(|| false),
    )
    .await
    .unwrap();
    server.await.unwrap();
    assert_eq!(out.destination, Destination::Assistant);
    assert_eq!(out.text, "request");
    assert!(events.lock().unwrap().iter().any(|e| matches!(
        e,
        Event::State {
            state: "Skipped",
            ..
        }
    )));
    assert!(g.nodes.iter().any(|n| n.id == router));
}

#[tokio::test]
async fn graph_runs_ready_llms_concurrently_and_streams_to_display() {
    let (mut s, mut g) = fixture();
    g.nodes.retain(|n| n.transcription());
    g.edges.clear();
    let source = g.nodes[0].id.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    s.model_registry
        .connections
        .push(crate::models::ModelConnection {
            id: "stream".into(),
            provider: "openai_compatible".into(),
            endpoint: format!("http://{address}/v1"),
            api_key: "".into(),
            api_version: "".into(),
            custom_header_name: "".into(),
            custom_header_value: "".into(),
            allow_lan_http: false,
        });
    let mut model =
        crate::models::ConfiguredModel::new("stream".into(), "Model".into(), "test".into());
    model.id = "llm".into();
    model.verified = true;
    s.model_registry.models.push(model);
    let llm = || Kind::Llm {
        step: Step::new(StepKind::Model {
            model_id: "llm".into(),
            name: "LLM".into(),
            instructions: "Inline instructions".into(),
            max_tokens: 16,
            temperature: None,
            timeout_seconds: 5,
        }),
        history: false,
        tools: vec![],
        assistant_response: false,
    };
    let a = add(&mut g, llm());
    let b = add(&mut g, llm());
    let instructions = add(
        &mut g,
        Kind::Text {
            text: "Wired instructions".into(),
        },
    );
    wire(&mut g, &instructions, "text", &b, "instructions");
    wire(&mut g, &source, "text", &a, "prompt");
    wire(&mut g, &source, "text", &b, "prompt");
    let end = add(&mut g, Kind::AssistantDisplay);
    wire(&mut g, &a, "text", &end, "text");
    let combine = add(
        &mut g,
        Kind::Combine {
            inputs: vec!["a".into(), "b".into()],
            separator: " / ".into(),
        },
    );
    wire(&mut g, &a, "text", &combine, "a");
    wire(&mut g, &b, "text", &combine, "b");
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let barrier = barrier.clone();
            requests.push(tokio::spawn(async move {
                use tokio::io::{AsyncReadExt,AsyncWriteExt};
                let mut request = Vec::new();
                loop {
                    let mut buf = [0;4096];
                    let count = socket.read(&mut buf).await.unwrap();
                    assert!(count>0);
                    request.extend_from_slice(&buf[..count]);
                    if let Some(end) = request.windows(4).position(|w|w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let length: usize = headers.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").map(|s|s.trim().parse().unwrap())).unwrap();
                        if request.len() >= end+4+length {break;}
                    }
                }
                barrier.wait().await;
                let body="data: {\"id\":\"test\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Generated\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                String::from_utf8(request).unwrap()
            }));
        }
        let mut bodies = Vec::new();
        for request in requests {
            bodies.push(request.await.unwrap());
        }
        assert_eq!(
            bodies
                .iter()
                .filter(|b| b.contains("Inline instructions"))
                .count(),
            1
        );
        assert_eq!(
            bodies
                .iter()
                .filter(|b| b.contains("Wired instructions"))
                .count(),
            1
        );
    });
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = events.clone();
    let run = execute(
        &g,
        "Prompt".into(),
        &s,
        "".into(),
        Arc::new(move |e| capture.lock().unwrap().push(e)),
        Arc::new(|| false),
    );
    let out = tokio::time::timeout(std::time::Duration::from_secs(8), run)
        .await
        .unwrap()
        .unwrap();
    server.await.unwrap();
    assert_eq!(out.text, "Generated");
    let events = events.lock().unwrap();
    let streamed = events
        .iter()
        .position(|e| matches!(e,Event::Text{node,..} if node==&end))
        .unwrap();
    let complete = events
        .iter()
        .position(|e| matches!(e,Event::State{node,state:"Complete"} if node==&a))
        .unwrap();
    assert!(streamed < complete);
    assert!(events.iter().any(
        |e| matches!(e,Event::Text{node,text} if node==&combine && text=="Generated / Generated")
    ));
}

#[cfg(windows)]
#[test]
fn graph_duplicate_hotkeys_reject_aliases() {
    let (mut s, _) = fixture();
    s.pipelines = Some(legacy_migrate(&s));
    crate::graph_ui::upgrade(&mut s);
    let set = s.pipelines.as_mut().unwrap();
    let mut other = set.pipelines[0].clone();
    other.id = new_id();
    set.pipelines[0].graph.as_mut().unwrap().hotkey = "Ctrl+Space".into();
    other.graph.as_mut().unwrap().hotkey = "Control + Space".into();
    set.pipelines.push(other);
    assert!(set
        .validate()
        .unwrap_err()
        .contains("hotkey must be unique"));
}
