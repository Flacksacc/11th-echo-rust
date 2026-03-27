use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::error::Error;
use std::collections::VecDeque;
use tokio::sync::mpsc::{Receiver, UnboundedReceiver}; // Bounded receiver
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::connect_async;
use url::Url;
use base64::{Engine as _, engine::general_purpose};

const ELEVENLABS_WSS_URL: &str = "wss://api.elevenlabs.io/v1/speech-to-text/realtime";

pub struct ElevenLabsClient {
    api_key: String,
    model_id: String,
}

#[derive(Debug, Clone)]
pub enum ControlMessage {
    Start,
    Stop,
}

#[derive(Debug, Clone)]
pub enum TranscriptMessage {
    Partial(String),
    Committed(String),
    Error(String),
}

#[derive(Debug)]
enum WsEvent {
    SessionStarted,
    CommittedTranscriptReceived,
}

#[derive(Debug, PartialEq, Eq)]
enum ParsedIncoming {
    SessionStarted,
    PartialTranscript(String),
    CommittedTranscript(String),
    Error(String),
    Other,
}

fn parse_incoming_message(text: &str) -> ParsedIncoming {
    let parsed: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return ParsedIncoming::Error(format!("Invalid JSON: {}", e)),
    };

    let msg_type = parsed
        .get("message_type")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    match msg_type {
        "session_started" => ParsedIncoming::SessionStarted,
        "partial_transcript" => ParsedIncoming::PartialTranscript(
            parsed
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        ),
        "committed_transcript" => ParsedIncoming::CommittedTranscript(
            parsed
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        ),
        "auth_error" | "quota_exceeded" | "transcriber_error" | "input_error" | "error" | "invalid_request" => {
            ParsedIncoming::Error(parsed.to_string())
        }
        _ => ParsedIncoming::Other,
    }
}

fn audio_chunk_payload(chunk: &[i16], commit: bool) -> String {
    let byte_data: Vec<u8> = chunk.iter().flat_map(|&s| s.to_le_bytes().to_vec()).collect();
    let b64 = general_purpose::STANDARD.encode(&byte_data);
    if commit {
        json!({
            "message_type": "input_audio_chunk",
            "audio_base_64": b64,
            "sample_rate": 16000,
            "commit": true
        })
        .to_string()
    } else {
        json!({
            "message_type": "input_audio_chunk",
            "audio_base_64": b64,
            "sample_rate": 16000
        })
        .to_string()
    }
}

fn silence_chunk_payload(commit: bool) -> String {
    let silence = vec![0i16; 3200];
    audio_chunk_payload(&silence, commit)
}

impl ElevenLabsClient {
    pub fn new(api_key: String, model_id: String) -> Self {
        Self { api_key, model_id }
    }

    pub async fn run(
        &self,
        mut audio_rx: Receiver<Vec<i16>>,
        mut control_rx: UnboundedReceiver<ControlMessage>,
        text_tx: tokio::sync::mpsc::Sender<TranscriptMessage>,
        log_tx: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let url = Url::parse_with_params(
            ELEVENLABS_WSS_URL,
            &[
                ("model_id", self.model_id.as_str()),
                ("language_code", "en"),
                ("audio_format", "pcm_16000"),
                ("commit_strategy", "manual"),
            ],
        )?;

        macro_rules! emit {
            ($($arg:tt)*) => {{
                let msg = format!($($arg)*);
                println!("{}", msg);
                let _ = log_tx.send(msg);
            }};
        }

        emit!("🔌 Connecting to ElevenLabs: {}", url);

        let mut request = url.as_str().into_client_request()?;
        request
            .headers_mut()
            .insert("xi-api-key", self.api_key.parse()?);

        emit!("➡️ [API OUT] WebSocket CONNECT {}", url);
        let (ws_stream, response) = connect_async(request).await?;
        emit!(
            "⬅️ [API IN] WebSocket CONNECT status={} headers={:?}",
            response.status(),
            response.headers()
        );
        emit!("✅ Connected to ElevenLabs WebSocket");

        let (mut write, mut read) = ws_stream.split();
        let (evt_tx, mut evt_rx) = mpsc::unbounded_channel::<WsEvent>();

        let log_tx_read = log_tx.clone();
        let read_task = tokio::spawn(async move {
            macro_rules! emit_read {
                ($($arg:tt)*) => {{
                    let msg = format!($($arg)*);
                    println!("{}", msg);
                    let _ = log_tx_read.send(msg);
                }};
            }
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                        emit_read!("⬅️ [API IN] WS text: {}", text);
                        match parse_incoming_message(&text) {
                            ParsedIncoming::SessionStarted => {
                                emit_read!("✅ [API IN] session_started");
                                let _ = evt_tx.send(WsEvent::SessionStarted);
                            }
                            ParsedIncoming::PartialTranscript(content) => {
                                if !content.is_empty() {
                                    emit_read!("📝 [PARTIAL] {}", content);
                                    let _ = text_tx.send(TranscriptMessage::Partial(content)).await;
                                }
                            }
                            ParsedIncoming::CommittedTranscript(content) => {
                                emit_read!("📝 [COMMITTED] {}", content);
                                let _ = text_tx.send(TranscriptMessage::Committed(content)).await;
                                let _ = evt_tx.send(WsEvent::CommittedTranscriptReceived);
                            }
                            ParsedIncoming::Error(err_json) => {
                                emit_read!("❌ [API ERROR] {}", err_json);
                                let _ = text_tx.send(TranscriptMessage::Error(err_json)).await;
                            }
                            ParsedIncoming::Other => {}
                        }
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                        emit_read!("🔌 WebSocket Closed");
                        break;
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Ping(payload)) => {
                        emit_read!("⬅️ [API IN] WS ping {} bytes", payload.len());
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Pong(payload)) => {
                        emit_read!("⬅️ [API IN] WS pong {} bytes", payload.len());
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Binary(payload)) => {
                        emit_read!("⬅️ [API IN] WS binary {} bytes", payload.len());
                    }
                    Err(e) => {
                        emit_read!("❌ WebSocket Error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        });

        let mut session_ready = false;
        let mut accepting_audio = false;
        let mut awaiting_final_commit = false;
        let mut queued_audio: VecDeque<Vec<i16>> = VecDeque::new();
        loop {
            tokio::select! {
                Some(evt) = evt_rx.recv() => {
                    match evt {
                        WsEvent::SessionStarted => {
                            session_ready = true;
                            emit!("➡️ Session ready, flushing {} queued chunks", queued_audio.len());
                            while let Some(chunk) = queued_audio.pop_front() {
                                let payload = audio_chunk_payload(&chunk, false);
                                emit!(
                                    "➡️ [API OUT] WS audio chunk: samples={} payload_bytes={}",
                                    chunk.len(),
                                    payload.len()
                                );
                                if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(payload)).await {
                                    emit!("❌ Failed to flush queued audio: {}", e);
                                    break;
                                }
                            }
                        }
                        WsEvent::CommittedTranscriptReceived => {
                            if awaiting_final_commit {
                                emit!("➡️ Final committed transcript received, closing WebSocket");
                                let _ = write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                                break;
                            }
                        }
                    }
                }
                Some(cmd) = control_rx.recv() => {
                    match cmd {
                        ControlMessage::Start => {
                            accepting_audio = true;
                            emit!("➡️ [API OUT] Segment start requested");
                        }
                        ControlMessage::Stop => {
                            accepting_audio = false;
                            awaiting_final_commit = true;
                            emit!("➡️ [API OUT] Manual commit requested");

                            let pre_commit_1 = silence_chunk_payload(false);
                            emit!("➡️ [API OUT] WS silence chunk 1/2 (pre-commit)");
                            if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(pre_commit_1)).await {
                                emit!("❌ Failed to send pre-commit silence chunk 1: {}", e);
                                break;
                            }

                            let pre_commit_2 = silence_chunk_payload(false);
                            emit!("➡️ [API OUT] WS silence chunk 2/2 (pre-commit)");
                            if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(pre_commit_2)).await {
                                emit!("❌ Failed to send pre-commit silence chunk 2: {}", e);
                                break;
                            }

                            let commit_payload = silence_chunk_payload(true);
                            emit!("➡️ [API OUT] WS commit chunk");
                            if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(commit_payload)).await {
                                emit!("❌ Failed to send commit chunk: {}", e);
                                break;
                            }
                        }
                    }
                }
                maybe_chunk = audio_rx.recv() => {
                    match maybe_chunk {
                        Some(chunk) => {
                            if !accepting_audio {
                                continue;
                            }
                            if !session_ready {
                                queued_audio.push_back(chunk);
                            } else {
                                let payload = audio_chunk_payload(&chunk, false);
                                emit!(
                                    "➡️ [API OUT] WS audio chunk: samples={} payload_bytes={}",
                                    chunk.len(),
                                    payload.len()
                                );
                                if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(payload)).await {
                                    emit!("❌ Failed to send audio: {}", e);
                                    break;
                                }
                            }
                        }
                        None => {
                            emit!("➡️ [API OUT] Audio stream ended, forcing manual commit");
                            let pre_commit_1 = silence_chunk_payload(false);
                            emit!("➡️ [API OUT] WS silence chunk 1/2 (pre-commit)");
                            if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(pre_commit_1)).await {
                                emit!("❌ Failed to send pre-commit silence chunk 1 after audio close: {}", e);
                                break;
                            }

                            let pre_commit_2 = silence_chunk_payload(false);
                            emit!("➡️ [API OUT] WS silence chunk 2/2 (pre-commit)");
                            if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(pre_commit_2)).await {
                                emit!("❌ Failed to send pre-commit silence chunk 2 after audio close: {}", e);
                                break;
                            }

                            let commit_payload = silence_chunk_payload(true);
                            emit!("➡️ [API OUT] WS commit chunk");
                            if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(commit_payload)).await {
                                emit!("❌ Failed to send commit chunk after audio close: {}", e);
                                break;
                            }
                            break;
                        }
                    }
                }
            }
        }

        // Cleanup
        let _ = read_task.await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{audio_chunk_payload, parse_incoming_message, silence_chunk_payload, ParsedIncoming};

    #[test]
    fn parse_session_started_event() {
        let msg = r#"{"message_type":"session_started","session_id":"abc"}"#;
        assert_eq!(parse_incoming_message(msg), ParsedIncoming::SessionStarted);
    }

    #[test]
    fn parse_committed_transcript_event() {
        let msg = r#"{"message_type":"committed_transcript","text":"hello world"}"#;
        assert_eq!(
            parse_incoming_message(msg),
            ParsedIncoming::CommittedTranscript("hello world".to_string())
        );
    }

    #[test]
    fn parse_error_event() {
        let msg = r#"{"message_type":"input_error","error":"bad format"}"#;
        match parse_incoming_message(msg) {
            ParsedIncoming::Error(payload) => assert!(payload.contains("input_error")),
            _ => panic!("expected ParsedIncoming::Error"),
        }
    }

    #[test]
    fn parse_partial_transcript_event() {
        let msg = r#"{"message_type":"partial_transcript","text":"hello"}"#;
        assert_eq!(
            parse_incoming_message(msg),
            ParsedIncoming::PartialTranscript("hello".to_string())
        );
    }

    #[test]
    fn parse_unknown_event_as_other() {
        let msg = r#"{"message_type":"something_else","x":1}"#;
        assert_eq!(parse_incoming_message(msg), ParsedIncoming::Other);
    }

    #[test]
    fn parse_invalid_json_as_error() {
        let msg = "{this is not json";
        match parse_incoming_message(msg) {
            ParsedIncoming::Error(payload) => assert!(payload.contains("Invalid JSON")),
            _ => panic!("expected ParsedIncoming::Error"),
        }
    }

    #[test]
    fn audio_payload_has_expected_fields() {
        let payload = audio_chunk_payload(&[1, -2, 3, -4], false);
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["message_type"], "input_audio_chunk");
        assert_eq!(v["sample_rate"], 16000);
        assert!(v.get("audio_base_64").and_then(|x| x.as_str()).is_some());
        assert!(v.get("commit").is_none());
    }

    #[test]
    fn audio_payload_commit_flag_when_requested() {
        let payload = audio_chunk_payload(&[0, 0, 0, 0], true);
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["commit"], true);
    }

    #[test]
    fn silence_payload_is_commit_enabled_when_requested() {
        let payload = silence_chunk_payload(true);
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["message_type"], "input_audio_chunk");
        assert_eq!(v["sample_rate"], 16000);
        assert_eq!(v["commit"], true);
    }
}
