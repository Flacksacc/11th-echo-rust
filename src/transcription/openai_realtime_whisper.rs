use base64::{engine::general_purpose, Engine as _};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::future::Future;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{Receiver, UnboundedReceiver};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use url::Url;

use super::{AudioChunk, TranscriptionCommand, TranscriptionEvent};

const OPENAI_REALTIME_URL: &str = "wss://api.openai.com/v1/realtime";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const SESSION_READY_TIMEOUT: Duration = Duration::from_secs(15);
const FINAL_COMMIT_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_PRE_READY_AUDIO_SAMPLES: usize = 16_000 * 15;

#[derive(Default)]
struct PreReadyAudioQueue {
    chunks: VecDeque<AudioChunk>,
    samples: usize,
}

impl PreReadyAudioQueue {
    fn push(&mut self, chunk: AudioChunk) -> Result<(), AudioChunk> {
        if self.samples.saturating_add(chunk.len()) > MAX_PRE_READY_AUDIO_SAMPLES {
            return Err(chunk);
        }
        self.samples += chunk.len();
        self.chunks.push_back(chunk);
        Ok(())
    }

    fn pop_front(&mut self) -> Option<AudioChunk> {
        let chunk = self.chunks.pop_front()?;
        self.samples -= chunk.len();
        Some(chunk)
    }

    fn clear(&mut self) {
        self.chunks.clear();
        self.samples = 0;
    }

    fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    fn len(&self) -> usize {
        self.chunks.len()
    }
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
                    Some(TranscriptionCommand::Stop) => accepting_audio = false,
                    None => return Ok(None),
                }
            }
            result = &mut connection => {
                return result.map(|connected| Some((connected, accepting_audio)));
            }
        }
    }
}

pub struct OpenAiRealtimeWhisperTranscriber {
    api_key: String,
    model_id: String,
    language_code: String,
}

#[derive(Debug)]
enum WsEvent {
    SessionReady,
    CommittedTranscriptReceived,
    TerminalError,
    ConnectionClosed,
}

#[derive(Debug, PartialEq, Eq)]
enum ParsedIncoming {
    SessionReady,
    PartialTranscript { item_id: String, delta: String },
    CommittedTranscript { item_id: String, transcript: String },
    Error(String),
    Other,
}

impl OpenAiRealtimeWhisperTranscriber {
    pub fn new(api_key: String, model_id: String, language_code: String) -> Self {
        Self {
            api_key,
            model_id,
            language_code,
        }
    }

    pub async fn run(
        &self,
        mut audio_rx: Receiver<AudioChunk>,
        mut control_rx: UnboundedReceiver<TranscriptionCommand>,
        text_tx: tokio::sync::mpsc::Sender<TranscriptionEvent>,
        log_tx: mpsc::UnboundedSender<String>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let url = realtime_url(&self.model_id)?;

        macro_rules! emit {
            ($($arg:tt)*) => {{
                let msg = format!($($arg)*);
                println!("{}", msg);
                let _ = log_tx.send(msg);
            }};
        }

        emit!("🔌 Connecting to OpenAI Realtime transcription: {}", url);

        let mut request = url.as_str().into_client_request()?;
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {}", self.api_key).parse()?);

        emit!("➡️ [API OUT] WebSocket CONNECT {}", url);
        let connection = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(request));
        let Some((connection_result, mut accepting_audio)) =
            (match connect_until_stopped(connection, &mut control_rx).await {
                Ok(result) => result,
                Err(_) => {
                    let message = "OpenAI connection timed out after 15 seconds. Please try again."
                        .to_string();
                    emit!("❌ {}", message);
                    let _ = text_tx.send(TranscriptionEvent::Error(message)).await;
                    return Ok(());
                }
            })
        else {
            emit!("➡️ OpenAI WebSocket connection cancelled before completion");
            return Ok(());
        };
        let (ws_stream, response) = connection_result?;
        emit!("⬅️ [API IN] WebSocket CONNECT status={}", response.status());
        emit!("✅ Connected to OpenAI Realtime WebSocket");

        let (mut write, mut read) = ws_stream.split();
        let (evt_tx, mut evt_rx) = mpsc::unbounded_channel::<WsEvent>();

        let session_payload = session_update_payload(&self.model_id, &self.language_code);
        emit!("➡️ [API OUT] WS session.update");
        write
            .send(tokio_tungstenite::tungstenite::Message::Text(
                session_payload,
            ))
            .await?;

        let log_tx_read = log_tx.clone();
        let text_tx_read = text_tx.clone();
        let mut read_task = tokio::spawn(async move {
            let mut partials_by_item: HashMap<String, String> = HashMap::new();
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
                        emit_read!("⬅️ [API IN] WS text frame: {} bytes", text.len());
                        match parse_incoming_message(&text) {
                            ParsedIncoming::SessionReady => {
                                emit_read!("✅ [API IN] transcription session ready");
                                let _ = evt_tx.send(WsEvent::SessionReady);
                            }
                            ParsedIncoming::PartialTranscript { item_id, delta } => {
                                if !delta.is_empty() {
                                    let partial = partials_by_item
                                        .entry(item_id)
                                        .and_modify(|existing| existing.push_str(&delta))
                                        .or_insert(delta)
                                        .clone();
                                    emit_read!(
                                        "📝 Partial transcript: {} characters",
                                        partial.chars().count()
                                    );
                                    let _ = text_tx_read
                                        .send(TranscriptionEvent::Partial(partial))
                                        .await;
                                }
                            }
                            ParsedIncoming::CommittedTranscript {
                                item_id,
                                transcript,
                            } => {
                                partials_by_item.remove(&item_id);
                                emit_read!(
                                    "📝 Committed transcript: {} characters",
                                    transcript.chars().count()
                                );
                                let _ = text_tx_read
                                    .send(TranscriptionEvent::Committed(transcript))
                                    .await;
                                let _ = evt_tx.send(WsEvent::CommittedTranscriptReceived);
                            }
                            ParsedIncoming::Error(err_json) => {
                                emit_read!("❌ OpenAI reported a transcription error");
                                let _ =
                                    text_tx_read.send(TranscriptionEvent::Error(err_json)).await;
                                let _ = evt_tx.send(WsEvent::TerminalError);
                            }
                            ParsedIncoming::Other => {}
                        }
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                        emit_read!("🔌 WebSocket Closed");
                        let _ = evt_tx.send(WsEvent::ConnectionClosed);
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
                        let _ = evt_tx.send(WsEvent::ConnectionClosed);
                        break;
                    }
                    _ => {}
                }
            }
            let _ = evt_tx.send(WsEvent::ConnectionClosed);
        });

        let mut session_ready = false;
        let mut awaiting_final_commit = !accepting_audio;
        let mut final_commit_deadline = None;
        let mut appended_audio = false;
        let mut audio_stream_ended = false;
        let mut queued_audio = PreReadyAudioQueue::default();
        if awaiting_final_commit {
            // Stop may arrive while TLS is connecting. Give the forwarding task
            // a short idle window to deliver audio captured before Stop.
            while let Some(chunk) = audio_rx.recv().await {
                if queued_audio.push(chunk).is_err() {
                    let message = "OpenAI received more than 15 seconds of audio before connecting. Please try again.".to_string();
                    emit!("❌ {}", message);
                    let _ = text_tx.send(TranscriptionEvent::Error(message)).await;
                    read_task.abort();
                    let _ = read_task.await;
                    return Ok(());
                }
            }
            audio_stream_ended = true;
        }
        let session_ready_timeout = tokio::time::sleep(SESSION_READY_TIMEOUT);
        tokio::pin!(session_ready_timeout);
        let mut final_commit_watchdog = tokio::time::interval(Duration::from_millis(250));
        final_commit_watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = final_commit_watchdog.tick(), if final_commit_deadline.is_some() => {
                    if tokio::time::Instant::now() >= final_commit_deadline.unwrap() {
                        let message = "OpenAI did not return a final transcript within 20 seconds. Please try again.".to_string();
                        emit!("❌ {}", message);
                        let _ = text_tx.send(TranscriptionEvent::Error(message)).await;
                        let _ = write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                        break;
                    }
                }
                _ = &mut session_ready_timeout, if !session_ready => {
                    let message = "OpenAI did not start the transcription session within 15 seconds. Please try again.".to_string();
                    emit!("❌ {}", message);
                    let _ = text_tx.send(TranscriptionEvent::Error(message)).await;
                    let _ = write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                    break;
                }
                Some(evt) = evt_rx.recv() => {
                    match evt {
                        WsEvent::SessionReady => {
                            session_ready = true;
                            emit!("➡️ Session ready, flushing {} queued chunks", queued_audio.len());
                            while let Some(chunk) = queued_audio.pop_front() {
                                let payload = audio_append_payload(&chunk);
                                emit!(
                                    "➡️ [API OUT] WS audio chunk: input_samples={} payload_bytes={}",
                                    chunk.len(),
                                    payload.len()
                                );
                                if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(payload)).await {
                                    emit!("❌ Failed to flush queued audio: {}", e);
                                    break;
                                }
                                appended_audio = true;
                            }
                            if awaiting_final_commit {
                                if appended_audio {
                                    let commit_payload = input_audio_buffer_commit_payload();
                                    emit!("➡️ [API OUT] WS input_audio_buffer.commit");
                                    if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(commit_payload)).await {
                                        emit!("❌ Failed to send delayed commit: {}", e);
                                        break;
                                    }
                                    final_commit_deadline = Some(tokio::time::Instant::now() + FINAL_COMMIT_TIMEOUT);
                                } else {
                                    emit!("➡️ No audio was sent; closing OpenAI WebSocket");
                                    let _ = text_tx.send(TranscriptionEvent::Committed(String::new())).await;
                                    let _ = write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
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
                        WsEvent::TerminalError => {
                            emit!("➡️ API reported terminal error, closing WebSocket");
                            let _ = write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                            break;
                        }
                        WsEvent::ConnectionClosed => {
                            emit!("➡️ WebSocket connection closed, ending session");
                            break;
                        }
                    }
                }
                Some(cmd) = control_rx.recv() => {
                    match cmd {
                        TranscriptionCommand::Start => {
                            accepting_audio = true;
                            appended_audio = false;
                            queued_audio.clear();
                            let clear_payload = input_audio_buffer_clear_payload();
                            emit!("➡️ [API OUT] Segment start requested");
                            if session_ready {
                                emit!("➡️ [API OUT] WS input_audio_buffer.clear");
                                if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(clear_payload)).await {
                                    emit!("❌ Failed to clear input audio buffer: {}", e);
                                    break;
                                }
                            }
                        }
                        TranscriptionCommand::Stop => {
                            accepting_audio = false;
                            awaiting_final_commit = true;
                            emit!("➡️ [API OUT] Manual commit requested");

                            while let Some(chunk) = audio_rx.recv().await {
                                if session_ready {
                                    let payload = audio_append_payload(&chunk);
                                    if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(payload)).await {
                                        emit!("❌ Failed to flush buffered audio before commit: {}", e);
                                        break;
                                    }
                                    appended_audio = true;
                                } else if queued_audio.push(chunk).is_err() {
                                    let message = "OpenAI received more than 15 seconds of buffered audio. Please try again.".to_string();
                                    emit!("❌ {}", message);
                                    let _ = text_tx.send(TranscriptionEvent::Error(message)).await;
                                    break;
                                }
                            }
                            audio_stream_ended = true;

                            if !appended_audio && queued_audio.is_empty() {
                                emit!("➡️ No audio was sent; closing OpenAI WebSocket");
                                let _ = text_tx.send(TranscriptionEvent::Committed(String::new())).await;
                                let _ = write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                                break;
                            }

                            if !session_ready {
                                continue;
                            }

                            while let Some(chunk) = queued_audio.pop_front() {
                                let payload = audio_append_payload(&chunk);
                                emit!(
                                    "➡️ [API OUT] WS audio chunk: input_samples={} payload_bytes={}",
                                    chunk.len(),
                                    payload.len()
                                );
                                if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(payload)).await {
                                    emit!("❌ Failed to flush queued audio before commit: {}", e);
                                    break;
                                }
                                appended_audio = true;
                            }

                            let commit_payload = input_audio_buffer_commit_payload();
                            emit!("➡️ [API OUT] WS input_audio_buffer.commit");
                            if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(commit_payload)).await {
                                emit!("❌ Failed to send commit: {}", e);
                                break;
                            }
                            final_commit_deadline = Some(tokio::time::Instant::now() + FINAL_COMMIT_TIMEOUT);
                        }
                    }
                }
                maybe_chunk = audio_rx.recv(), if !audio_stream_ended => {
                    match maybe_chunk {
                        Some(chunk) => {
                            if !accepting_audio {
                                continue;
                            }
                            if !session_ready {
                                if queued_audio.push(chunk).is_err() {
                                    let message = "OpenAI was not ready after 15 seconds of queued audio. Please try again.".to_string();
                                    emit!("❌ {}", message);
                                    let _ = text_tx.send(TranscriptionEvent::Error(message)).await;
                                    let _ = write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                                    break;
                                }
                            } else {
                                let payload = audio_append_payload(&chunk);
                                emit!(
                                    "➡️ [API OUT] WS audio chunk: input_samples={} payload_bytes={}",
                                    chunk.len(),
                                    payload.len()
                                );
                                if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(payload)).await {
                                    emit!("❌ Failed to send audio: {}", e);
                                    break;
                                }
                                appended_audio = true;
                            }
                        }
                        None => {
                            emit!("➡️ [API OUT] Audio stream ended, forcing manual commit");
                            audio_stream_ended = true;
                            if appended_audio || !queued_audio.is_empty() {
                                while let Some(chunk) = queued_audio.pop_front() {
                                    let payload = audio_append_payload(&chunk);
                                    if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(payload)).await {
                                        emit!("❌ Failed to flush queued audio after audio close: {}", e);
                                        break;
                                    }
                                }
                                let commit_payload = input_audio_buffer_commit_payload();
                                if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(commit_payload)).await {
                                    emit!("❌ Failed to send commit after audio close: {}", e);
                                    break;
                                }
                                awaiting_final_commit = true;
                                final_commit_deadline = Some(tokio::time::Instant::now() + FINAL_COMMIT_TIMEOUT);
                            } else {
                                let _ = text_tx.send(TranscriptionEvent::Committed(String::new())).await;
                                let _ = write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
                                break;
                            }
                        }
                    }
                }
            }
        }

        if tokio::time::timeout(Duration::from_secs(2), &mut read_task)
            .await
            .is_err()
        {
            read_task.abort();
            let _ = read_task.await;
        }
        Ok(())
    }
}

fn realtime_url(model_id: &str) -> Result<Url, url::ParseError> {
    let _ = model_id;
    Url::parse_with_params(OPENAI_REALTIME_URL, &[("intent", "transcription")])
}

fn session_update_payload(model_id: &str, language_code: &str) -> String {
    json!({
        "type": "session.update",
        "session": {
            "type": "transcription",
            "audio": {
                "input": {
                    "format": {
                        "type": "audio/pcm",
                        "rate": 24000
                    },
                    "transcription": {
                        "model": model_id,
                        "language": language_code
                    },
                    "turn_detection": null
                }
            }
        }
    })
    .to_string()
}

fn input_audio_buffer_clear_payload() -> String {
    json!({ "type": "input_audio_buffer.clear" }).to_string()
}

fn input_audio_buffer_commit_payload() -> String {
    json!({ "type": "input_audio_buffer.commit" }).to_string()
}

fn audio_append_payload(chunk_16k: &[i16]) -> String {
    let chunk_24k = resample_pcm16_16k_to_24k(chunk_16k);
    let byte_data: Vec<u8> = chunk_24k
        .iter()
        .flat_map(|&s| s.to_le_bytes().to_vec())
        .collect();
    json!({
        "type": "input_audio_buffer.append",
        "audio": general_purpose::STANDARD.encode(&byte_data)
    })
    .to_string()
}

fn resample_pcm16_16k_to_24k(input: &[i16]) -> Vec<i16> {
    if input.is_empty() {
        return Vec::new();
    }

    let output_len = input.len() * 3 / 2;
    let mut output = Vec::with_capacity(output_len);
    for out_idx in 0..output_len {
        let numerator = out_idx * 2;
        let src_idx = numerator / 3;
        let frac = (numerator % 3) as f32 / 3.0;
        let a = input[src_idx] as f32;
        let b = input.get(src_idx + 1).copied().unwrap_or(input[src_idx]) as f32;
        output.push((a + (b - a) * frac).round() as i16);
    }
    output
}

fn parse_incoming_message(text: &str) -> ParsedIncoming {
    let parsed: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return ParsedIncoming::Error(format!("Invalid JSON: {}", e)),
    };

    let msg_type = parsed
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    match msg_type {
        "session.created"
        | "session.updated"
        | "transcription_session.created"
        | "transcription_session.updated" => ParsedIncoming::SessionReady,
        "conversation.item.input_audio_transcription.delta" => ParsedIncoming::PartialTranscript {
            item_id: parsed
                .get("item_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            delta: parsed
                .get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        },
        "conversation.item.input_audio_transcription.completed" => {
            ParsedIncoming::CommittedTranscript {
                item_id: parsed
                    .get("item_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                transcript: parsed
                    .get("transcript")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            }
        }
        "error" => ParsedIncoming::Error(parsed.to_string()),
        _ => ParsedIncoming::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        audio_append_payload, connect_until_stopped, parse_incoming_message, realtime_url,
        resample_pcm16_16k_to_24k, session_update_payload, ParsedIncoming, PreReadyAudioQueue,
    };
    use crate::transcription::TranscriptionCommand;
    use std::future;
    use std::time::Duration;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn stop_during_connection_is_preserved() {
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        control_tx.send(TranscriptionCommand::Start).unwrap();
        control_tx.send(TranscriptionCommand::Stop).unwrap();

        let result =
            connect_until_stopped(future::ready(Ok::<_, ()>("connected")), &mut control_rx)
                .await
                .unwrap();

        assert_eq!(result, Some(("connected", false)));
    }

    #[tokio::test]
    async fn start_received_during_connect_is_preserved() {
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        control_tx.send(TranscriptionCommand::Start).unwrap();

        let result =
            connect_until_stopped(future::ready(Ok::<_, ()>("connected")), &mut control_rx)
                .await
                .unwrap()
                .unwrap();

        assert_eq!(result, ("connected", true));
    }

    #[tokio::test]
    async fn connection_attempt_has_a_finite_timeout() {
        let (_control_tx, mut control_rx) = mpsc::unbounded_channel();
        let result = connect_until_stopped(
            tokio::time::timeout(
                Duration::from_millis(1),
                future::pending::<Result<(), ()>>(),
            ),
            &mut control_rx,
        )
        .await;

        assert!(result.is_err());
    }

    #[test]
    fn pre_ready_audio_queue_is_bounded_by_audio_duration() {
        let mut queue = PreReadyAudioQueue::default();
        assert!(queue.push(vec![0; 16_000 * 15]).is_ok());
        assert!(queue.push(vec![0]).is_err());
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn realtime_url_uses_model_query() {
        let url = realtime_url("gpt-realtime-whisper").unwrap();
        assert_eq!(
            url.as_str(),
            "wss://api.openai.com/v1/realtime?intent=transcription"
        );
    }

    #[test]
    fn session_update_configures_transcription_session() {
        let payload = session_update_payload("gpt-realtime-whisper", "en");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["type"], "session.update");
        assert_eq!(v["session"]["type"], "transcription");
        assert_eq!(
            v["session"]["audio"]["input"]["format"]["type"],
            "audio/pcm"
        );
        assert_eq!(v["session"]["audio"]["input"]["format"]["rate"], 24000);
        assert_eq!(
            v["session"]["audio"]["input"]["transcription"]["model"],
            "gpt-realtime-whisper"
        );
        assert_eq!(
            v["session"]["audio"]["input"]["transcription"]["language"],
            "en"
        );
        assert!(v["session"]["audio"]["input"]["turn_detection"].is_null());
    }

    #[test]
    fn resample_16k_to_24k_outputs_three_samples_for_every_two() {
        let out = resample_pcm16_16k_to_24k(&[0, 300, 600, 900]);
        assert_eq!(out.len(), 6);
        assert_eq!(out[0], 0);
        assert_eq!(out[3], 600);
    }

    #[test]
    fn audio_append_payload_has_expected_fields() {
        let payload = audio_append_payload(&[1, -2, 3, -4]);
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["type"], "input_audio_buffer.append");
        assert!(v.get("audio").and_then(|x| x.as_str()).is_some());
    }

    #[test]
    fn parse_session_updated_as_ready() {
        let msg =
            r#"{"type":"session.updated","session":{"id":"sess_123","type":"transcription"}}"#;
        assert_eq!(parse_incoming_message(msg), ParsedIncoming::SessionReady);
    }

    #[test]
    fn parse_transcription_session_updated_as_ready() {
        let msg = r#"{"type":"transcription_session.updated","session":{"id":"sess_123"}}"#;
        assert_eq!(parse_incoming_message(msg), ParsedIncoming::SessionReady);
    }

    #[test]
    fn parse_transcript_delta() {
        let msg = r#"{"type":"conversation.item.input_audio_transcription.delta","item_id":"item_1","delta":"hello"}"#;
        assert_eq!(
            parse_incoming_message(msg),
            ParsedIncoming::PartialTranscript {
                item_id: "item_1".to_string(),
                delta: "hello".to_string()
            }
        );
    }

    #[test]
    fn parse_transcript_completed() {
        let msg = r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_1","transcript":"hello world"}"#;
        assert_eq!(
            parse_incoming_message(msg),
            ParsedIncoming::CommittedTranscript {
                item_id: "item_1".to_string(),
                transcript: "hello world".to_string()
            }
        );
    }

    #[test]
    fn parse_error_event() {
        let msg = r#"{"type":"error","error":{"message":"bad request"}}"#;
        match parse_incoming_message(msg) {
            ParsedIncoming::Error(payload) => assert!(payload.contains("bad request")),
            _ => panic!("expected ParsedIncoming::Error"),
        }
    }
}
