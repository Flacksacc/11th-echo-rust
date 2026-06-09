use base64::{engine::general_purpose, Engine as _};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::collections::VecDeque;
use std::error::Error;
use std::future::Future;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{Receiver, UnboundedReceiver}; // Bounded receiver
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use url::Url;

use super::{AudioChunk, TranscriptionCommand, TranscriptionEvent};

const ELEVENLABS_WSS_URL: &str = "wss://api.elevenlabs.io/v1/speech-to-text/realtime";

pub struct ElevenLabsRealtimeTranscriber {
    api_key: String,
    model_id: String,
    language_code: String,
    no_verbatim: bool,
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
        "auth_error" | "quota_exceeded" | "resource_exhausted" | "transcriber_error"
        | "input_error" | "error" | "invalid_request" => ParsedIncoming::Error(parsed.to_string()),
        _ => ParsedIncoming::Other,
    }
}

fn audio_chunk_payload(chunk: &[i16], commit: bool) -> String {
    let byte_data: Vec<u8> = chunk
        .iter()
        .flat_map(|&s| s.to_le_bytes().to_vec())
        .collect();
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

fn realtime_url(
    model_id: &str,
    language_code: &str,
    no_verbatim: bool,
) -> Result<Url, url::ParseError> {
    let no_verbatim = if no_verbatim { "true" } else { "false" };
    Url::parse_with_params(
        ELEVENLABS_WSS_URL,
        &[
            ("model_id", model_id),
            ("language_code", language_code),
            ("audio_format", "pcm_16000"),
            ("commit_strategy", "manual"),
            ("no_verbatim", no_verbatim),
        ],
    )
}

fn silence_chunk_payload(commit: bool) -> String {
    let silence = vec![0i16; 3200];
    audio_chunk_payload(&silence, commit)
}

async fn connect_until_stopped<F, T, E>(
    connection: F,
    control_rx: &mut UnboundedReceiver<TranscriptionCommand>,
) -> Result<Option<(T, bool)>, E>
where
    F: Future<Output = Result<T, E>>,
{
    tokio::pin!(connection);
    let mut accepting_audio = false;

    loop {
        tokio::select! {
            biased;

            command = control_rx.recv() => {
                match command {
                    Some(TranscriptionCommand::Start) => accepting_audio = true,
                    Some(TranscriptionCommand::Stop) | None => return Ok(None),
                }
            }
            result = &mut connection => {
                return result.map(|connected| Some((connected, accepting_audio)));
            }
        }
    }
}

impl ElevenLabsRealtimeTranscriber {
    pub fn new(
        api_key: String,
        model_id: String,
        language_code: String,
        no_verbatim: bool,
    ) -> Self {
        Self {
            api_key,
            model_id,
            language_code,
            no_verbatim,
        }
    }

    pub async fn run(
        &self,
        mut audio_rx: Receiver<AudioChunk>,
        mut control_rx: UnboundedReceiver<TranscriptionCommand>,
        text_tx: tokio::sync::mpsc::Sender<TranscriptionEvent>,
        log_tx: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let url = realtime_url(&self.model_id, &self.language_code, self.no_verbatim)?;

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
        let Some(((ws_stream, response), mut accepting_audio)) =
            connect_until_stopped(connect_async(request), &mut control_rx).await?
        else {
            emit!("➡️ WebSocket connection cancelled before completion");
            return Ok(());
        };
        emit!(
            "⬅️ [API IN] WebSocket CONNECT status={} headers={:?}",
            response.status(),
            response.headers()
        );
        emit!("✅ Connected to ElevenLabs WebSocket");

        let (mut write, mut read) = ws_stream.split();

        let mut session_ready = false;
        let mut awaiting_final_commit = false;
        let mut queued_audio: VecDeque<Vec<i16>> = VecDeque::new();
        loop {
            tokio::select! {
                message = read.next() => {
                    match message {
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                            emit!("⬅️ [API IN] WS text: {}", text);
                            match parse_incoming_message(&text) {
                                ParsedIncoming::SessionStarted => {
                                    session_ready = true;
                                    emit!("✅ [API IN] session_started");
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
                                ParsedIncoming::PartialTranscript(content) => {
                                    if !content.is_empty() {
                                        emit!("📝 [PARTIAL] {}", content);
                                        let _ = text_tx.send(TranscriptionEvent::Partial(content)).await;
                                    }
                                }
                                ParsedIncoming::CommittedTranscript(content) => {
                                    emit!("📝 [COMMITTED] {}", content);
                                    let _ = text_tx.send(TranscriptionEvent::Committed(content)).await;
                                    if awaiting_final_commit {
                                        emit!("➡️ Final committed transcript received, closing WebSocket");
                                        let _ = write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                                        break;
                                    }
                                }
                                ParsedIncoming::Error(err_json) => {
                                    emit!("❌ [API ERROR] {}", err_json);
                                    let _ = text_tx.send(TranscriptionEvent::Error(err_json)).await;
                                    emit!("➡️ API reported terminal error, closing WebSocket");
                                    let _ = write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                                    break;
                                }
                                ParsedIncoming::Other => {}
                            }
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => {
                            emit!("🔌 WebSocket Closed");
                            break;
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(payload))) => {
                            emit!("⬅️ [API IN] WS ping {} bytes", payload.len());
                            if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Pong(payload)).await {
                                emit!("❌ Failed to send WebSocket pong: {}", e);
                                break;
                            }
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Pong(payload))) => {
                            emit!("⬅️ [API IN] WS pong {} bytes", payload.len());
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(payload))) => {
                            emit!("⬅️ [API IN] WS binary {} bytes", payload.len());
                        }
                        Some(Err(e)) => {
                            emit!("❌ WebSocket Error: {}", e);
                            break;
                        }
                        Some(Ok(_)) => {}
                    }
                }
                Some(cmd) = control_rx.recv() => {
                    match cmd {
                        TranscriptionCommand::Start => {
                            accepting_audio = true;
                            emit!("➡️ [API OUT] Segment start requested");
                        }
                        TranscriptionCommand::Stop => {
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

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        audio_chunk_payload, connect_until_stopped, parse_incoming_message, realtime_url,
        silence_chunk_payload, ParsedIncoming,
    };
    use crate::transcription::TranscriptionCommand;
    use std::future;
    use std::time::Duration;
    use tokio::sync::mpsc;

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

    #[test]
    fn realtime_url_requests_non_verbatim_transcripts() {
        let url = realtime_url("scribe_v2_realtime", "en", true).unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(
            query.get("model_id").map(String::as_str),
            Some("scribe_v2_realtime")
        );
        assert_eq!(query.get("language_code").map(String::as_str), Some("en"));
        assert_eq!(
            query.get("audio_format").map(String::as_str),
            Some("pcm_16000")
        );
        assert_eq!(
            query.get("commit_strategy").map(String::as_str),
            Some("manual")
        );
        assert_eq!(query.get("no_verbatim").map(String::as_str), Some("true"));
    }

    #[tokio::test]
    async fn stop_cancels_connection_attempt() {
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        control_tx.send(TranscriptionCommand::Start).unwrap();
        control_tx.send(TranscriptionCommand::Stop).unwrap();

        let result = tokio::time::timeout(
            Duration::from_millis(100),
            connect_until_stopped(future::pending::<Result<(), ()>>(), &mut control_rx),
        )
        .await
        .expect("stop should cancel a pending connection")
        .unwrap();

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn start_received_during_connect_is_preserved() {
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        control_tx.send(TranscriptionCommand::Start).unwrap();

        let result =
            connect_until_stopped(future::ready(Ok::<_, ()>("connected")), &mut control_rx)
                .await
                .unwrap();

        assert_eq!(result, Some(("connected", true)));
    }
}
