use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::error::Error;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{Receiver, UnboundedReceiver}; // Bounded receiver
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use url::Url;
use base64::{Engine as _, engine::general_purpose};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

const ELEVENLABS_WSS_URL: &str = "wss://api.elevenlabs.io/v1/speech-to-text/realtime";

#[derive(Serialize)]
struct AudioMessage {
    audio_event: AudioEvent,
}

#[derive(Serialize)]
struct AudioEvent {
    audio_base_64: String,
    event_type: String, // "audio_input"
}

#[derive(Deserialize, Debug)]
struct TranscriptEvent {
    #[serde(rename = "type")]
    event_type: String, // "partial_transcript" | "final_transcript"
    text: Option<String>,
    is_final: Option<bool>,
}

pub struct ElevenLabsClient {
    api_key: String,
    model_id: String,
}

#[derive(Debug, Clone)]
pub enum ControlMessage {
    Stop,
}

impl ElevenLabsClient {
    pub fn new(api_key: String, model_id: String) -> Self {
        Self { api_key, model_id }
    }

    pub async fn run(
        &self,
        mut audio_rx: Receiver<Vec<i16>>,
        mut control_rx: UnboundedReceiver<ControlMessage>,
        text_tx: tokio::sync::mpsc::Sender<String>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let url = Url::parse_with_params(
            ELEVENLABS_WSS_URL,
            &[
                ("model_id", self.model_id.as_str()),
                //("language_code", "en"), 
                ("audio_format", "pcm_16000"),
            ],
        )?;

        println!("🔌 Connecting to ElevenLabs: {}", url);
        
        let request = http::Request::builder()
            .uri(url.as_str())
            .header("xi-api-key", &self.api_key)
            .body(())?;

        let (ws_stream, _) = connect_async(request).await?;
        println!("✅ Connected to ElevenLabs WebSocket");

        let (mut write, mut read) = ws_stream.split();

        // Spawn a task to read from WS and send text to injector
        let read_task = tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&text) {
                            if let Some(msg_type) = event.get("type").and_then(|v| v.as_str()) {
                                match msg_type {
                                    "partial_transcript" => {
                                        // Handle partials if needed
                                    },
                                    "final_transcript" => {
                                        if let Some(content) = event.get("text").and_then(|v| v.as_str()) {
                                            if !content.is_empty() {
                                                println!("📝 Transcript: {}", content);
                                                let _ = text_tx.send(content.to_string()).await;
                                            }
                                        }
                                    },
                                    _ => {}
                                }
                            }
                        }
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                        println!("🔌 WebSocket Closed");
                        break;
                    }
                    Err(e) => {
                        eprintln!("❌ WebSocket Error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        });

        // Loop to send audio from channel to WS and handle stop/finalize signals.
        let mut sent_end_stream = false;
        loop {
            tokio::select! {
                Some(cmd) = control_rx.recv() => {
                    if matches!(cmd, ControlMessage::Stop) && !sent_end_stream {
                        let end_stream_msg = json!({ "type": "end_stream" });
                        if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(end_stream_msg.to_string())).await {
                            eprintln!("❌ Failed to send end_stream: {}", e);
                            break;
                        }
                        sent_end_stream = true;
                        println!("📨 Sent end_stream to ElevenLabs");
                        break;
                    }
                }
                maybe_chunk = audio_rx.recv() => {
                    match maybe_chunk {
                        Some(chunk) => {
                            let byte_data: Vec<u8> = chunk.iter().flat_map(|&s| s.to_le_bytes().to_vec()).collect();
                            let b64 = general_purpose::STANDARD.encode(&byte_data);

                            // Correct Scribe v2 JSON format
                            let valid_msg = json!({
                                "type": "audio",
                                "data": b64
                            });

                            if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(valid_msg.to_string())).await {
                                eprintln!("❌ Failed to send audio: {}", e);
                                break;
                            }
                        }
                        None => {
                            if !sent_end_stream {
                                let end_stream_msg = json!({ "type": "end_stream" });
                                if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(end_stream_msg.to_string())).await {
                                    eprintln!("❌ Failed to send end_stream after audio closed: {}", e);
                                } else {
                                    println!("📨 Sent end_stream to ElevenLabs after audio capture ended");
                                }
                                sent_end_stream = true;
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
