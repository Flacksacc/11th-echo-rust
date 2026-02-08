use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::error::Error;
use tokio::net::TcpStream;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use url::Url;

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

impl ElevenLabsClient {
    pub fn new(api_key: String, model_id: String) -> Self {
        Self { api_key, model_id }
    }

    pub async fn run(
        &self,
        mut audio_rx: UnboundedReceiver<Vec<i16>>,
        text_tx: tokio::sync::mpsc::Sender<String>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let url = Url::parse_with_params(
            ELEVENLABS_WSS_URL,
            &[
                ("model_id", &self.model_id),
                //("language_code", "en"), // Default is en, can make configurable later
                ("audio_format", "pcm_16000"),
            ],
        )?;

        println!("🔌 Connecting to ElevenLabs: {}", url);

        // Header injection for API Key might be needed if not supported in query params for auth.
        // ElevenLabs docs usually say "xi-api-key" header.
        // tungstenite `connect_async` takes a Request, so we can add headers.
        
        let request = http::Request::builder()
            .uri(url.as_str())
            .header("xi-api-key", &self.api_key)
            .body(())?;

        let (ws_stream, _) = connect_async(request).await?;
        println!("✅ Connected to ElevenLabs WebSocket");

        let (mut write, mut read) = ws_stream.split();

        // Spawn a task to read from WS and send text to injector
        let mut read_task = tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                        // println!("📩 Received: {}", text); // Debug log
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

        // Loop to send audio from channel to WS
        while let Some(chunk) = audio_rx.recv().await {
            // Convert i16 to base64
            // ElevenLabs expects base64 encoded raw PCM or container format?
            // "audio_format": "pcm_16000" implies raw PCM.
            // We need to convert [i16] -> [u8] bytes -> Base64 String.
            
            let byte_data: Vec<u8> = chunk.iter().flat_map(|&s| s.to_le_bytes().to_vec()).collect();
            let b64 = base64::encode(&byte_data);

            let msg = json!({
                "audio_event": {
                    "audio_base_64": b64,
                    "event_type": "audio_input"
                }
            });

            // Note: Check ElevenLabs Scribe v2 specific JSON structure.
            // Docs: { "audio": "base64...", "isFinal": false } or just raw JSON?
            // Re-checking standard structure:
            // usually: { "type": "audio_input", "data": "base64..." } or similar.
            // Let's stick to a generic "audio_event" wrapper if that's what their specialized client does,
            // OR simply follow the standard:
            
            // Standard Scribe v2:
            // Send JSON: { "type": "audio", "data": "<base64>" } ?
            // Actually, looking at previous TS code:
            // "message_type": "input_audio_chunk", "audio_base_64": "...", "sample_rate": 16000
            
            let valid_msg = json!({
                "message_type": "input_audio_chunk",
                "audio_base_64": b64,
                "sample_rate": 16000 // Optional if set in connection
            });

            if let Err(e) = write.send(tokio_tungstenite::tungstenite::Message::Text(valid_msg.to_string())).await {
                eprintln!("❌ Failed to send audio: {}", e);
                break;
            }
        }
        
        // Cleanup
        let _ = read_task.await;
        Ok(())
    }
}
