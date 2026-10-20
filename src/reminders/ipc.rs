use super::*;
fn name() -> String {
    use sha2::{Digest, Sha256};
    format!(
        "\\\\.\\pipe\\Echo.Reminders.{:x}",
        Sha256::digest(directory().to_string_lossy().as_bytes())
    )
}
#[cfg(windows)]
pub fn start(service: Arc<Service>) {
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let Ok(mut pipe) = crate::avatar_bridge::private_pipe(&name()) else {
                return;
            };
            while !service.stop.load(Ordering::SeqCst) {
                let result =
                    tokio::time::timeout(std::time::Duration::from_millis(200), pipe.connect())
                        .await;
                if !matches!(result, Ok(Ok(()))) {
                    continue;
                }
                let exchange = async {
                    let len = pipe.read_u32_le().await? as usize;
                    if len > 32768 {
                        return Err(std::io::Error::other("Request too large"));
                    }
                    let mut bytes = vec![0; len];
                    pipe.read_exact(&mut bytes).await?;
                    let result = serde_json::from_slice::<Value>(&bytes)
                        .map_err(|_| "Invalid request".into())
                        .and_then(|args| service.command(args));
                    let value = match result {
                        Ok(v) => v,
                        Err(e) => json!({"error":e}),
                    };
                    let bytes = serde_json::to_vec(&value)?;
                    pipe.write_u32_le(bytes.len() as u32).await?;
                    pipe.write_all(&bytes).await?;
                    Ok::<_, std::io::Error>(())
                };
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), exchange).await;
                let _ = pipe.disconnect();
            }
        });
    });
}
#[cfg(not(windows))]
pub fn start(_: Arc<Service>) {}
pub fn broker() -> Result<(), String> {
    use std::io::{Read, Write};
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(32769)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read reminder command.")?;
    if bytes.len() > 32768 {
        return Err("Reminder command exceeds its budget.".into());
    }
    #[cfg(windows)]
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "Could not start reminder broker.")?;
        let value = rt.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(8), async {
                let mut pipe = loop {
                    match tokio::net::windows::named_pipe::ClientOptions::new().open(name()) {
                        Ok(pipe) => break pipe,
                        Err(e) if e.raw_os_error() == Some(231) => {
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await
                        }
                        Err(_) => return Err("Echo must be running to manage reminders."),
                    }
                };
                pipe.write_u32_le(bytes.len() as u32)
                    .await
                    .map_err(|_| "Reminder broker failed.")?;
                pipe.write_all(&bytes)
                    .await
                    .map_err(|_| "Reminder broker failed.")?;
                let len = pipe
                    .read_u32_le()
                    .await
                    .map_err(|_| "Reminder broker failed.")? as usize;
                if len > 2 * 1024 * 1024 {
                    return Err("Reminder reply exceeded its budget.");
                }
                let mut result = vec![0; len];
                pipe.read_exact(&mut result)
                    .await
                    .map_err(|_| "Reminder broker failed.")?;
                Ok(result)
            })
            .await
            .map_err(|_| "Echo's reminder broker timed out.")?
        })?;
        std::io::stdout()
            .write_all(&value)
            .map_err(|_| "Could not return reminder reply.")?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = bytes;
        Err("Reminder IPC requires Windows.".into())
    }
}
pub fn cli() -> Result<(), String> {
    broker()
}
