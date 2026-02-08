mod injector;
mod audio;
mod network;

use slint::ComponentHandle;
use std::thread;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

#[cfg(target_os = "windows")]
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    TrayIconBuilder, TrayIconEvent,
};
#[cfg(target_os = "windows")]
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager,
};

slint::include_modules!();

// Command protocol between UI and Tokio Runtime
enum AppCommand {
    StartRecording { api_key: String, model: String },
    StopRecording,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    println!("🦋 11th Echo Rust (Iron Butterfly) Starting...");

    // 1. Setup Global Hotkeys
    #[cfg(target_os = "windows")]
    let (manager, hotkey) = {
        let manager = GlobalHotKeyManager::new().unwrap();
        let hotkey = HotKey::new(Some(Modifiers::CONTROL), Code::Space);
        manager.register(hotkey).unwrap();
        (manager, hotkey)
    };

    // 2. Setup Tray Icon
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
        (quit_item.id(), show_item.id(), Some(tray))
    };

    // Channel to send commands from UI to Tokio
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<AppCommand>();

    // Channel to send audio levels from Tokio to UI
    let (level_tx, mut level_rx) = mpsc::channel::<f32>(10);

    let ui = AppWindow::new()?;
    let ui_handle_for_tokio = ui.as_weak();

    // Spawn the async runtime in a separate thread
    thread::spawn(move || {
        let rt = Runtime::new().unwrap();
        rt.block_on(async move {
            println!("⚡ Tokio Runtime Active");
            
            struct Session {
                _stop_tx: oneshot::Sender<()>,
                audio_stream: cpal::Stream,
            }
            
            let mut active_session: Option<Session> = None;

            loop {
                tokio::select! {
                    Some(level) = level_rx.recv() => {
                        let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                            ui.set_audio_level(level);
                        });
                    }
                    Some(cmd) = cmd_rx.recv() => {
                        match cmd {
                            AppCommand::StartRecording { api_key, model } => {
                                println!("⚡ Starting Recording Session...");
                                let (audio_tx, audio_rx) = mpsc::channel::<Vec<i16>>(50); 
                                let (text_tx, mut text_rx) = mpsc::channel::<String>(100);
                                let (stop_tx, stop_rx) = oneshot::channel::<()>();
                                let audio_level_tx = level_tx.clone();

                                let stream_result = audio::start_audio_capture(audio_tx, audio_level_tx);
                                match stream_result {
                                    Ok(stream) => {
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
                                            println!("⚡ Session Tear-down");
                                        });

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
                                active_session = None;
                                let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                    ui.set_audio_level(0.0);
                                });
                            }
                        }
                    }
                }
            }
        });
    });

    // Connect signals
    let ui_handle = ui.as_weak();
    let start_tx = cmd_tx.clone();
    
    ui.on_start_recording(move |api_key_slint| {
        let ui = ui_handle.unwrap();
        let api_key = api_key_slint.to_string();
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

    // Polling timer for hotkeys and tray events
    let ui_handle_for_timer = ui.as_weak();
    let cmd_tx_for_timer = cmd_tx.clone();
    
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(50), move || {
        if let Some(ui) = ui_handle_for_timer.upgrade() {
            #[cfg(target_os = "windows")]
            {
                // Global Hotkeys
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
                                model: "scribe_v2".to_string()
                            });
                            ui.set_is_recording(true);
                        }
                        ui.show().unwrap();
                    }
                }

                // Tray Events
                while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                    println!("tray event: {event:?}");
                }

                // Menu Events
                while let Ok(event) = MenuEvent::receiver().try_recv() {
                    if event.id == quit_item_id {
                        slint::quit_event_loop().unwrap();
                    } else if event.id == show_item_id {
                        ui.show().unwrap();
                    }
                }
            }
        }
    });

    ui.run()?;
    Ok(())
}
