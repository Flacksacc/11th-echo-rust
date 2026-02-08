mod injector;
mod audio;
mod network;

use slint::ComponentHandle;
use std::thread;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

slint::include_modules!();

// Command protocol between UI and Tokio Runtime
enum AppCommand {
    StartRecording { api_key: String, model: String },
    StopRecording,
}

fn main() -> Result<(), slint::PlatformError> {
    env_logger::init();
    println!("🦋 11th Echo Rust (Iron Butterfly) Starting...");

    // Channel to send commands from UI to Tokio
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<AppCommand>();

    // Spawn the async runtime in a separate thread
    thread::spawn(move || {
        let rt = Runtime::new().unwrap();
        rt.block_on(async move {
            println!("⚡ Tokio Runtime Active");
            
            // State for the active recording session
            struct Session {
                _stop_tx: oneshot::Sender<()>, // Dropping this will stop the task
                audio_stream: cpal::Stream,    // Dropping this stops audio
            }
            
            let mut active_session: Option<Session> = None;

            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    AppCommand::StartRecording { api_key, model } => {
                        println!("⚡ Starting Recording Session...");
                        
                        // 1. Create channels
                        let (audio_tx, audio_rx) = mpsc::unbounded_channel::<Vec<i16>>();
                        let (text_tx, mut text_rx) = mpsc::channel::<String>(100);
                        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();

                        // 2. Start Audio Capture (Sync)
                        let stream_result = audio::start_audio_capture(audio_tx);
                        match stream_result {
                            Ok(stream) => {
                                // 3. Spawn Network Client (Async)
                                let client = network::ElevenLabsClient::new(api_key, model);
                                tokio::spawn(async move {
                                    tokio::select! {
                                        _ = client.run(audio_rx, text_tx) => {
                                            println!("⚡ Network client finished");
                                        }
                                        _ = stop_rx => {
                                            println!("⚡ Stop signal received");
                                        }
                                    }
                                });

                                // 4. Spawn Text Injector (Async listener)
                                tokio::spawn(async move {
                                    while let Some(text) = text_rx.recv().await {
                                        println!("⌨️ Injecting: {}", text);
                                        if let Err(e) = injector::inject_text(&text) {
                                            eprintln!("❌ Injection Error: {}", e);
                                        }
                                    }
                                });

                                active_session = Some(Session {
                                    _stop_tx: stop_tx,
                                    audio_stream: stream,
                                });
                                println!("✅ Session Active");
                            },
                            Err(e) => {
                                eprintln!("❌ Failed to start audio: {}", e);
                            }
                        }
                    },
                    AppCommand::StopRecording => {
                        println!("⚡ Stopping Recording Session...");
                        active_session = None; // Drop session -> drops stream & stop_tx
                    }
                }
            }
        });
    });

    let ui = AppWindow::new()?;

    // Connect signals
    let ui_handle = ui.as_weak();
    let start_tx = cmd_tx.clone();
    
    ui.on_start_recording(move || {
        let ui = ui_handle.unwrap();
        // Mock API Key fetch - in real app, bind to UI property
        // For now, hardcode or grab from env if we could, but UI has LineEdit
        // We'll just send placeholder
        let api_key = "sk_mock_key".to_string(); 
        
        let _ = start_tx.send(AppCommand::StartRecording { 
            api_key, 
            model: "scribe_v2".to_string() 
        });
        
        ui.set_is_recording(true);
    });

    let ui_handle = ui.as_weak();
    let stop_tx = cmd_tx.clone();
    
    ui.on_stop_recording(move || {
        let ui = ui_handle.unwrap();
        let _ = stop_tx.send(AppCommand::StopRecording);
        ui.set_is_recording(false);
    });

    ui.run()
}
