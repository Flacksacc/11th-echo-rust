use futures_util::StreamExt;
use minisign_verify::{PublicKey, Signature};
use reqwest::{redirect, Client, Response};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use url::Url;

const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_SIGNATURE_BYTES: usize = 8 * 1024;
const MAX_RELEASE_NOTES_BYTES: usize = 20 * 1024;
const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct UpdateConfig {
    pub feed_url: Url,
    pub public_key: PublicKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateManifestV1 {
    pub schema_version: u32,
    pub channel: String,
    pub version: Version,
    pub published_at: String,
    #[serde(default)]
    pub release_notes: Option<String>,
    pub installer: InstallerManifest,
    #[serde(default)]
    pub authenticode_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallerManifest {
    pub file: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub manifest: UpdateManifestV1,
    pub installer_url: Url,
}

pub fn compiled_config() -> Result<Option<UpdateConfig>, String> {
    let Some(feed_url) = option_env!("ECHO_UPDATE_FEED_URL") else {
        return Ok(None);
    };
    let Some(public_key) = option_env!("ECHO_UPDATE_PUBLIC_KEY") else {
        return Err("The update feed is configured without a public verification key".into());
    };
    parse_config(feed_url, public_key).map(Some)
}

fn parse_config(feed_url: &str, public_key: &str) -> Result<UpdateConfig, String> {
    let feed_url =
        Url::parse(feed_url.trim()).map_err(|err| format!("Invalid update URL: {err}"))?;
    ensure_https(&feed_url)?;
    if feed_url.cannot_be_a_base() || feed_url.host_str().is_none() {
        return Err("The update feed URL must be an absolute HTTPS URL".into());
    }
    let encoded_key = public_key
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("untrusted comment:"))
        .ok_or_else(|| "The update public key is empty".to_string())?;
    let public_key = PublicKey::from_base64(encoded_key)
        .map_err(|err| format!("Invalid update public key: {err}"))?;
    Ok(UpdateConfig {
        feed_url,
        public_key,
    })
}

pub async fn check_for_update(
    config: &UpdateConfig,
    current_version: &Version,
) -> Result<Option<UpdateInfo>, String> {
    let client = update_client()?;
    let manifest_response =
        get_limited(&client, config.feed_url.clone(), MAX_MANIFEST_BYTES).await?;
    let final_manifest_url = manifest_response.0;
    let manifest_bytes = manifest_response.1;

    let signature_url = signature_url(&final_manifest_url)?;
    let (_, signature_bytes) = get_limited(&client, signature_url, MAX_SIGNATURE_BYTES).await?;
    let signature_text = std::str::from_utf8(&signature_bytes)
        .map_err(|_| "The update signature is not valid UTF-8".to_string())?;
    let signature = Signature::decode(signature_text)
        .map_err(|err| format!("Invalid update signature: {err}"))?;
    config
        .public_key
        .verify(&manifest_bytes[..], &signature, false)
        .map_err(|err| format!("Update signature verification failed: {err}"))?;

    let manifest: UpdateManifestV1 = serde_json::from_slice(&manifest_bytes)
        .map_err(|err| format!("Invalid update manifest: {err}"))?;
    validate_manifest(&manifest)?;
    if manifest.version <= *current_version {
        return Ok(None);
    }
    let installer_url = final_manifest_url
        .join(&manifest.installer.file)
        .map_err(|err| format!("Invalid installer URL: {err}"))?;
    ensure_https(&installer_url)?;
    Ok(Some(UpdateInfo {
        manifest,
        installer_url,
    }))
}

pub async fn download_update<F>(info: &UpdateInfo, progress: F) -> Result<PathBuf, String>
where
    F: Fn(f32) + Send + Sync,
{
    let update_dir = update_directory()?;
    tokio::fs::create_dir_all(&update_dir)
        .await
        .map_err(|err| format!("Could not create the update directory: {err}"))?;
    clean_stale_partials(&update_dir);

    let destination = update_dir.join(&info.manifest.installer.file);
    let partial = destination.with_extension("exe.partial");
    let client = update_client()?;
    let response = client
        .get(info.installer_url.clone())
        .send()
        .await
        .map_err(|err| format!("Could not download the update: {err}"))?;
    ensure_success_https(&response)?;
    if let Some(length) = response.content_length() {
        if length != info.manifest.installer.size {
            return Err(format!(
                "Update size mismatch: expected {} bytes, server reported {length}",
                info.manifest.installer.size
            ));
        }
    }

    let mut file = tokio::fs::File::create(&partial)
        .await
        .map_err(|err| format!("Could not create the update file: {err}"))?;
    let mut stream = response.bytes_stream();
    let mut hasher = Sha256::new();
    let mut downloaded = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| format!("Update download was interrupted: {err}"))?;
        downloaded = downloaded
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| "The update download is too large".to_string())?;
        if downloaded > info.manifest.installer.size || downloaded > MAX_INSTALLER_BYTES {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err("The update download exceeded its declared size".into());
        }
        file.write_all(&chunk)
            .await
            .map_err(|err| format!("Could not write the update file: {err}"))?;
        hasher.update(&chunk);
        progress(downloaded as f32 / info.manifest.installer.size as f32);
    }
    file.flush()
        .await
        .map_err(|err| format!("Could not flush the update file: {err}"))?;
    drop(file);

    if downloaded != info.manifest.installer.size {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(format!(
            "Update download was incomplete: expected {} bytes, received {downloaded}",
            info.manifest.installer.size
        ));
    }
    let actual_hash = format!("{:x}", hasher.finalize());
    if !actual_hash.eq_ignore_ascii_case(&info.manifest.installer.sha256) {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err("Update SHA-256 verification failed".into());
    }
    if info.manifest.authenticode_required {
        verify_authenticode(&partial)?;
    }
    if destination.exists() {
        tokio::fs::remove_file(&destination)
            .await
            .map_err(|err| format!("Could not replace an older update: {err}"))?;
    }
    tokio::fs::rename(&partial, &destination)
        .await
        .map_err(|err| format!("Could not finalize the update file: {err}"))?;
    Ok(destination)
}

pub fn launch_installer(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Err("The verified update installer is missing".into());
    }
    std::process::Command::new(path)
        .args([
            "/SILENT",
            "/SP-",
            "/CLOSEAPPLICATIONS",
            "/NORESTART",
            "/UPDATE",
        ])
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("Could not start the update installer: {err}"))
}

fn validate_manifest(manifest: &UpdateManifestV1) -> Result<(), String> {
    if manifest.schema_version != 1 {
        return Err(format!(
            "Unsupported update manifest schema {}",
            manifest.schema_version
        ));
    }
    if manifest.channel != "stable" {
        return Err("The update manifest is not for the stable channel".into());
    }
    chrono::DateTime::parse_from_rfc3339(&manifest.published_at)
        .map_err(|_| "The update publication timestamp is invalid".to_string())?;
    if !manifest.version.pre.is_empty() {
        return Err("Prerelease versions are not accepted on the stable channel".into());
    }
    if manifest.installer.size == 0 || manifest.installer.size > MAX_INSTALLER_BYTES {
        return Err("The declared installer size is invalid".into());
    }
    if !is_safe_basename(&manifest.installer.file) {
        return Err("The installer filename must be a simple .exe basename".into());
    }
    if manifest.installer.sha256.len() != 64
        || !manifest
            .installer
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("The installer SHA-256 value is invalid".into());
    }
    if manifest
        .release_notes
        .as_ref()
        .is_some_and(|notes| notes.len() > MAX_RELEASE_NOTES_BYTES)
    {
        return Err("The release notes are too large".into());
    }
    Ok(())
}

fn is_safe_basename(name: &str) -> bool {
    !name.is_empty()
        && name.to_ascii_lowercase().ends_with(".exe")
        && Path::new(name).file_name().and_then(|value| value.to_str()) == Some(name)
        && !name.contains(['/', '\\'])
        && name != "."
        && name != ".."
}

fn signature_url(manifest_url: &Url) -> Result<Url, String> {
    let mut url = manifest_url.clone();
    let path = format!("{}.minisig", url.path());
    url.set_path(&path);
    Ok(url)
}

fn update_client() -> Result<Client, String> {
    let policy = redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 3 {
            return attempt.error("too many update redirects");
        }
        if attempt.url().scheme() != "https" {
            return attempt.error("update redirects must remain on HTTPS");
        }
        attempt.follow()
    });
    Client::builder()
        .redirect(policy)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .user_agent(format!("Echo/{} updater", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|err| format!("Could not initialize the updater: {err}"))
}

async fn get_limited(client: &Client, url: Url, limit: usize) -> Result<(Url, Vec<u8>), String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| format!("Could not contact the update server: {err}"))?;
    ensure_success_https(&response)?;
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("The update server response is too large".into());
    }
    let final_url = response.url().clone();
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|err| format!("Could not read the update server response: {err}"))?;
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err("The update server response is too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((final_url, bytes))
}

fn ensure_success_https(response: &Response) -> Result<(), String> {
    ensure_https(response.url())?;
    if !response.status().is_success() {
        return Err(format!(
            "The update server returned HTTP {}",
            response.status()
        ));
    }
    Ok(())
}

fn ensure_https(url: &Url) -> Result<(), String> {
    if url.scheme() != "https" {
        return Err("Updates require HTTPS".into());
    }
    Ok(())
}

fn update_directory() -> Result<PathBuf, String> {
    dirs_next::data_local_dir()
        .map(|path| path.join("11th_echo").join("updates"))
        .ok_or_else(|| "Could not determine the local update directory".into())
}

fn clean_stale_partials(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("partial") {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(target_os = "windows")]
fn verify_authenticode(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Security::WinTrust::{
        WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0,
        WINTRUST_FILE_INFO, WTD_CHOICE_FILE, WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT,
        WTD_REVOKE_WHOLECHAIN, WTD_STATEACTION_IGNORE, WTD_UI_NONE,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(wide.as_ptr()),
        hFile: Default::default(),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: &mut file_info,
        },
        dwStateAction: WTD_STATEACTION_IGNORE,
        dwProvFlags: WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status =
        unsafe { WinVerifyTrust(HWND::default(), &mut action, &mut data as *mut _ as *mut _) };
    if status == 0 {
        Ok(())
    } else {
        Err(format!(
            "Authenticode verification failed with status 0x{status:08X}"
        ))
    }
}

#[cfg(not(target_os = "windows"))]
fn verify_authenticode(_path: &Path) -> Result<(), String> {
    Err("Authenticode verification is only supported on Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> UpdateManifestV1 {
        UpdateManifestV1 {
            schema_version: 1,
            channel: "stable".into(),
            version: Version::parse("1.2.3").unwrap(),
            published_at: "2026-07-31T12:00:00Z".into(),
            release_notes: Some("Safer updates".into()),
            installer: InstallerManifest {
                file: "Echo-1.2.3-Setup.exe".into(),
                size: 1234,
                sha256: "a".repeat(64),
            },
            authenticode_required: false,
        }
    }

    #[test]
    fn accepts_valid_stable_manifest() {
        assert!(validate_manifest(&manifest()).is_ok());
    }

    #[test]
    fn rejects_traversal_and_non_executable_names() {
        for name in ["../update.exe", "folder/update.exe", "update.zip", ""] {
            let mut candidate = manifest();
            candidate.installer.file = name.into();
            assert!(validate_manifest(&candidate).is_err(), "accepted {name}");
        }
    }

    #[test]
    fn rejects_prereleases_and_invalid_hashes() {
        let mut candidate = manifest();
        candidate.version = Version::parse("2.0.0-beta.1").unwrap();
        assert!(validate_manifest(&candidate).is_err());
        candidate.version = Version::parse("2.0.0").unwrap();
        candidate.installer.sha256 = "not-a-hash".into();
        assert!(validate_manifest(&candidate).is_err());
    }

    #[test]
    fn rejects_non_https_config_before_parsing_key() {
        let error =
            parse_config("http://updates.example.com/manifest.json", "invalid").unwrap_err();
        assert!(error.contains("HTTPS"));
    }

    #[test]
    fn accepts_standard_minisign_public_key_file_text() {
        let key = "untrusted comment: minisign public key\n\
                   RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        assert!(parse_config("https://updates.example.com/manifest.json", key).is_ok());
    }
}
