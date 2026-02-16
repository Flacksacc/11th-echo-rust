mod injector;
mod audio;
mod network;
mod state;

use slint::ComponentHandle;
use std::sync::{Arc, Mutex};
use std::thread;
use state::RecordingState;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

#[cfg(target_os = "windows")]
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager,
};
#[cfg(target_os = "windows")]
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    TrayIconBuilder, TrayIconEvent,
};

slint::include_modules!();

#[derive(Debug)]
enum AppCommand {
    StartRecording { api_key: String, model: String },
    StopRecording,
}

struct Session {
    state: Arc<Mutex<RecordingState>>,
    audio_stream: Option<cpal::Stream>,
    network_stop_tx: Option<mpsc::UnboundedSender<network::ControlMessage>>,
}

impl Session {
    fn stop_capture(&mut self) {
        // Dropping CPAL stream closes the capture callback and drops the audio sender,
        // allowing the network task to drain and receive final transcript events.
        self.audio_stream.take();
    }

    fn stop_network(&mut self) {
        if let Some(tx) = self.network_stop_tx.take() {
            let _ = tx.send(network::ControlMessage::Stop);
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    println!("🦋 11th Echo Rust (Iron Butterfly) Starting...");

    #[cfg(target_os = "windows")]
    let (_manager, hotkey) = {
        let manager = GlobalHotKeyManager::new().unwrap();
        let hotkey = HotKey::new(Some(Modifiers::CONTROL), Code::Space);
        manager.register(hotkey).unwrap();
        (manager, hotkey)
    };

    #[cfg(target_os = "windows")]
    let (quit_item_id, show_item_id, _tray_icon) = {
        let tray_menu = Menu::new();
        let show_item = MenuItem::new("Show Settings", true, None);
        let quit_item = MenuItem::new("Quit", true, None);
        tray_menu.append_items(&[&show_item, &quit_item])?;

        let icon = tray_icon::Icon::from_path("eleventhecho.ico", None)?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip("11th Echo")
            .with_icon(icon)
            .build()?;
        (quit_item.id().clone(), show_item.id().clone(), Some(tray))
    };

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<AppCommand>();
    let (level_tx, mut level_rx) = mpsc::channel::<f32>(10);

    let ui = AppWindow::new()?;
    let ui_handle_for_tokio = ui.as_weak();

    thread::spawn(move || {
        let rt = Runtime::new().unwrap();
        rt.block_on(async move {
            println!("⚡ Tokio Runtime Active");

            let mut active_session: Option<Session> = None;
            let (finalize_tx, mut finalize_rx) = mpsc::unbounded_channel::<()>();

            loop {
                tokio::select! {
                    Some(level) = level_rx.recv() => {
                        let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                            ui.set_audio_level(level);
                        });
                    }
                    Some(()) = finalize_rx.recv() => {
                        if let Some(session) = active_session.take() {
                            if let Ok(mut state) = session.state.lock() {
                                state.transition_to_idle();
                            }
                            println!("✅ Finalization complete, session closed");
                        }
                        let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                            ui.set_audio_level(0.0);
                        });
                    }
                    Some(cmd) = cmd_rx.recv() => {
                        match cmd {
                            AppCommand::StartRecording { api_key, model } => {
                                if let Some(ref session) = active_session {
                                    if !session.state.lock().unwrap().can_start() {
                                        println!("❌ Cannot start recording: session already active");
                                        continue;
                                    }
                                }

                                println!("⚡ Starting Recording Session...");
                                let state = Arc::new(Mutex::new(RecordingState::BufferingPreConnect));

                                let (audio_tx, audio_rx) = mpsc::channel::<Vec<i16>>(50);
                                let (network_stop_tx, network_stop_rx) =
                                    mpsc::unbounded_channel::<network::ControlMessage>();
                                let (text_tx, mut text_rx) = mpsc::channel::<String>(100);
                                let audio_level_tx = level_tx.clone();

                                let stream_result = audio::start_audio_capture(audio_tx, audio_level_tx);
                                match stream_result {
                                    Ok(stream) => {
                                        let client = network::ElevenLabsClient::new(api_key, model);
                                        let client_state = state.clone();
                                        let injection_state = state.clone();
                                        let finalize_tx_for_network = finalize_tx.clone();
                                        let finalize_tx_for_injection = finalize_tx.clone();
                                        let ui_handle_for_transcript = ui_handle_for_tokio.clone();

                                        tokio::spawn(async move {
                                            {
                                                let mut s = client_state.lock().unwrap();
                                                s.transition_to_connecting();
                                            }

                                            let result = client.run(audio_rx, network_stop_rx, text_tx).await;
                                            if let Err(err) = result {
                                                eprintln!("❌ Network client failed: {}", err);
                                                if let Ok(mut s) = client_state.lock() {
                                                    *s = RecordingState::Error("Network client failed".to_string());
                                                }
                                                let _ = finalize_tx_for_network.send(());
                                                return;
                                            }

                                            if let Ok(mut s) = client_state.lock() {
                                                if matches!(
                                                    *s,
                                                    RecordingState::BufferingPreConnect
                                                        | RecordingState::Connecting
                                                        | RecordingState::Recording
                                                ) {
                                                    s.transition_to_finalizing();
                                                }
                                            }

                                            println!("⚡ Network client task ended");
                                        });

                                        tokio::spawn(async move {
                                            while let Some(text) = text_rx.recv().await {
                                                // Update UI with transcript immediately
                                                let text_clone = text.clone();
                                                let _ = ui_handle_for_transcript.upgrade_in_event_loop(move |ui| {
                                                    let current = ui.get_transcript();
                                                    let new_text = if current.len() > 0 {
                                                        format!("{}\n{}", current, text_clone)
                                                    } else {
                                                        text_clone
                                                    };
                                                    ui.set_transcript(new_text.into());
                                                });

                                                {
                                                    let mut s = injection_state.lock().unwrap();
                                                    s.transition_to_recording();
                                                    s.begin_injection();
                                                }

                                                println!("⌨️ Injecting: {}", text);
                                                
                                                if let Err(e) = injector::inject_text(&text) {
                                                    eprintln!("❌ Injection Error: {}", e);
                                                }

                                                let mut s = injection_state.lock().unwrap();
                                                s.finish_injection();
                                            }

                                            let should_finalize = {
                                                let s = injection_state.lock().unwrap();
                                                matches!(
                                                    *s,
                                                    RecordingState::Finalizing {
                                                        pending_injections: 0
                                                    }
                                                )
                                            };

                                            if should_finalize {
                                                let _ = finalize_tx_for_injection.send(());
                                            }
                                        });

                                        active_session = Some(Session {
                                            state,
                                            audio_stream: Some(stream),
                                            network_stop_tx: Some(network_stop_tx),
                                        });
                                        println!("✅ Session Active");
                                    }
                                    Err(e) => {
                                        eprintln!("❌ Failed to start audio: {}", e);
                                    }
                                }
                            }
                            AppCommand::StopRecording => {
                                println!("⚡ Stop requested");
                                if let Some(session) = active_session.as_mut() {
                                    {
                                        let mut s = session.state.lock().unwrap();
                                        if s.can_stop() {
                                            s.transition_to_finalizing();
                                        } else if s.is_finalizing() {
                                            println!("📝 Already finalizing");
                                        } else {
                                            println!("ℹ️ Stop ignored in state: {:?}", *s);
                                        }
                                    }
                                    session.stop_network();
                                    session.stop_capture();
                                } else {
                                    println!("ℹ️ No active session to stop");
                                }
                            }
                        }
                    }
                }
            }
        });
    });

    let ui_handle = ui.as_weak();
    let start_tx = cmd_tx.clone();

    ui.on_start_recording(move |api_key_slint| {
        let ui = ui_handle.unwrap();
        let api_key = api_key_slint.to_string();
        let _ = start_tx.send(AppCommand::StartRecording {
            api_key,
            model: "scribe_v2".to_string(),
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

    let ui_handle_for_timer = ui.as_weak();
    let cmd_tx_for_timer = cmd_tx.clone();

    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(50),
        move || {
            if let Some(ui) = ui_handle_for_timer.upgrade() {
                #[cfg(target_os = "windows")]
                {
                    while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
                        if event.id == hotkey.id() {
                            println!("🔥 Hotkey Pressed!");
                            if ui.get_is_recording() {
                                let _ = cmd_tx_for_timer.send(AppCommand::StopRecording);
                                ui.set_is_recording(false);
                            } else {
                                let api_key = ui.get_api_key_text().to_string();
                                let _ = cmd_tx_for_timer.send(AppCommand::StartRecording {
                                    api_key,
                                    model: "scribe_v2".to_string(),
                                });
                                ui.set_is_recording(true);
                            }
                            ui.show().unwrap();
                        }
                    }

                    while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                        println!("tray event: {event:?}");
                    }

                    while let Ok(event) = MenuEvent::receiver().try_recv() {
                        if event.id == quit_item_id {
                            slint::quit_event_loop().unwrap();
                        } else if event.id == show_item_id {
                            ui.show().unwrap();
                        }
                    }
                }
            }
        },
    );

    ui.run()?;
    Ok(())
}
