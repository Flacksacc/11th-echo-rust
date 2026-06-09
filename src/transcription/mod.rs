use std::error::Error;
use tokio::sync::mpsc::{Receiver, Sender, UnboundedReceiver, UnboundedSender};

mod elevenlabs_realtime;
mod openai_realtime_whisper;

pub use elevenlabs_realtime::ElevenLabsRealtimeTranscriber;
pub use openai_realtime_whisper::OpenAiRealtimeWhisperTranscriber;

pub const DEFAULT_PROVIDER_ID: &str = "elevenlabs_realtime";
pub const DEFAULT_PROVIDER_LABEL: &str = "ElevenLabs Realtime";
pub const OPENAI_REALTIME_WHISPER_PROVIDER_ID: &str = "openai_realtime_whisper";
pub const OPENAI_REALTIME_WHISPER_PROVIDER_LABEL: &str = "OpenAI Realtime Whisper";
pub const DEFAULT_ELEVENLABS_REALTIME_MODEL_ID: &str = "scribe_v2_realtime";
pub const DEFAULT_OPENAI_REALTIME_WHISPER_MODEL_ID: &str = "gpt-realtime-whisper";
pub const DEFAULT_LANGUAGE_CODE: &str = "en";

pub type AudioChunk = Vec<i16>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptionProvider {
    ElevenLabsRealtime,
    OpenAiRealtimeWhisper,
}

impl TranscriptionProvider {
    pub fn from_id(id: &str) -> Self {
        match id.trim() {
            DEFAULT_PROVIDER_ID | DEFAULT_PROVIDER_LABEL | "" => Self::ElevenLabsRealtime,
            OPENAI_REALTIME_WHISPER_PROVIDER_ID | OPENAI_REALTIME_WHISPER_PROVIDER_LABEL => {
                Self::OpenAiRealtimeWhisper
            }
            _ => Self::ElevenLabsRealtime,
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            Self::ElevenLabsRealtime => DEFAULT_PROVIDER_ID,
            Self::OpenAiRealtimeWhisper => OPENAI_REALTIME_WHISPER_PROVIDER_ID,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::ElevenLabsRealtime => DEFAULT_PROVIDER_LABEL,
            Self::OpenAiRealtimeWhisper => OPENAI_REALTIME_WHISPER_PROVIDER_LABEL,
        }
    }

    pub fn default_model_id(&self) -> &'static str {
        match self {
            Self::ElevenLabsRealtime => DEFAULT_ELEVENLABS_REALTIME_MODEL_ID,
            Self::OpenAiRealtimeWhisper => DEFAULT_OPENAI_REALTIME_WHISPER_MODEL_ID,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TranscriptionConfig {
    pub provider: TranscriptionProvider,
    pub api_key: String,
    pub model_id: String,
    pub language_code: String,
    pub no_verbatim: bool,
}

#[derive(Debug, Clone)]
pub enum TranscriptionCommand {
    Start,
    Stop,
}

#[derive(Debug, Clone)]
pub enum TranscriptionEvent {
    Partial(String),
    Committed(String),
    Error(String),
}

pub enum TranscriberClient {
    ElevenLabsRealtime(ElevenLabsRealtimeTranscriber),
    OpenAiRealtimeWhisper(OpenAiRealtimeWhisperTranscriber),
}

impl TranscriberClient {
    pub fn from_config(config: TranscriptionConfig) -> Self {
        match config.provider {
            TranscriptionProvider::ElevenLabsRealtime => {
                Self::ElevenLabsRealtime(ElevenLabsRealtimeTranscriber::new(
                    config.api_key,
                    config.model_id,
                    config.language_code,
                    config.no_verbatim,
                ))
            }
            TranscriptionProvider::OpenAiRealtimeWhisper => {
                Self::OpenAiRealtimeWhisper(OpenAiRealtimeWhisperTranscriber::new(
                    config.api_key,
                    config.model_id,
                    config.language_code,
                ))
            }
        }
    }

    pub async fn run(
        &self,
        audio_rx: Receiver<AudioChunk>,
        command_rx: UnboundedReceiver<TranscriptionCommand>,
        event_tx: Sender<TranscriptionEvent>,
        log_tx: UnboundedSender<String>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        match self {
            Self::ElevenLabsRealtime(client) => {
                client.run(audio_rx, command_rx, event_tx, log_tx).await
            }
            Self::OpenAiRealtimeWhisper(client) => {
                client.run(audio_rx, command_rx, event_tx, log_tx).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        TranscriberClient, TranscriptionConfig, TranscriptionProvider,
        DEFAULT_ELEVENLABS_REALTIME_MODEL_ID, DEFAULT_LANGUAGE_CODE,
        DEFAULT_OPENAI_REALTIME_WHISPER_MODEL_ID,
    };

    #[test]
    fn provider_defaults_to_elevenlabs_realtime() {
        assert_eq!(
            TranscriptionProvider::from_id("").id(),
            "elevenlabs_realtime"
        );
        assert_eq!(
            TranscriptionProvider::from_id("unknown-provider").id(),
            "elevenlabs_realtime"
        );
        assert_eq!(
            TranscriptionProvider::from_id("OpenAI Realtime Whisper").id(),
            "openai_realtime_whisper"
        );
    }

    #[test]
    fn factory_builds_default_elevenlabs_client() {
        let client = TranscriberClient::from_config(TranscriptionConfig {
            provider: TranscriptionProvider::ElevenLabsRealtime,
            api_key: "sk_test".to_string(),
            model_id: DEFAULT_ELEVENLABS_REALTIME_MODEL_ID.to_string(),
            language_code: DEFAULT_LANGUAGE_CODE.to_string(),
            no_verbatim: true,
        });

        assert!(matches!(client, TranscriberClient::ElevenLabsRealtime(_)));
    }

    #[test]
    fn factory_builds_openai_realtime_whisper_client() {
        let client = TranscriberClient::from_config(TranscriptionConfig {
            provider: TranscriptionProvider::OpenAiRealtimeWhisper,
            api_key: "sk_test".to_string(),
            model_id: DEFAULT_OPENAI_REALTIME_WHISPER_MODEL_ID.to_string(),
            language_code: DEFAULT_LANGUAGE_CODE.to_string(),
            no_verbatim: true,
        });

        assert!(matches!(
            client,
            TranscriberClient::OpenAiRealtimeWhisper(_)
        ));
    }
}
