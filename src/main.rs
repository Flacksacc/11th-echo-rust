mod injector;
mod audio;

use slint::ComponentHandle;
use std::thread;
use tokio::runtime::Runtime;

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    env_logger::init();
    println!("🦋 11th Echo Rust (Iron Butterfly) Starting...");

    // Spawn the async runtime in a separate thread
    thread::spawn(|| {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            println!("⚡ Tokio Runtime Active");
            // Future: Audio/Network actors go here
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            }
        });
    });

    let ui = AppWindow::new()?;

    // Connect signals
    let ui_handle = ui.as_weak();
    ui.on_start_recording(move || {
        let ui = ui_handle.unwrap();
        ui.set_is_recording(true);
        println!("🔴 Recording Started (Mock)");
        
        // Here we would signal the Tokio thread to start audio capture
    });

    let ui_handle = ui.as_weak();
    ui.on_stop_recording(move || {
        let ui = ui_handle.unwrap();
        ui.set_is_recording(false);
        println!("⚪ Recording Stopped (Mock)");
    });

    ui.run()
}
