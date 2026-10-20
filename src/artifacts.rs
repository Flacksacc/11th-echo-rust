//! Managed response attachments. Model output never grants filesystem authority.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub name: String,
    pub media_type: String,
    pub path: PathBuf,
    pub generated: bool,
}
static REGISTRY: LazyLock<Mutex<HashMap<String, Artifact>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
pub fn root() -> PathBuf {
    crate::settings::settings_path().with_file_name("assistant_outputs")
}
pub fn register(path: &Path, generated: bool, media_type: String) -> Result<Artifact, String> {
    let path = path
        .canonicalize()
        .map_err(|_| "Attachment file is unavailable.")?;
    if !path.is_file() {
        return Err("Attachment must be a file.".into());
    }
    let file = Artifact {
        id: crate::pipelines::new_id(),
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into(),
        path,
        generated,
        media_type,
    };
    REGISTRY
        .lock()
        .unwrap()
        .insert(file.id.clone(), file.clone());
    Ok(file)
}
pub fn lookup(id: &str) -> Result<Artifact, String> {
    REGISTRY
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(|| "Unknown attachment ID; obtain the file through a tool first.".into())
}
pub fn materialize(specs: &[crate::response_channels::FileSpec]) -> Result<Vec<Artifact>, String> {
    materialize_at(specs, &root())
}
fn materialize_at(
    specs: &[crate::response_channels::FileSpec],
    storage: &Path,
) -> Result<Vec<Artifact>, String> {
    if specs.len() > 32 {
        return Err("Too many attachments.".into());
    }
    // Validate the whole manifest before writing any files.
    for spec in specs {
        match spec {
            crate::response_channels::FileSpec::Existing { artifact_id } => {
                lookup(artifact_id)?;
            }
            crate::response_channels::FileSpec::Generated { name, content, .. } => {
                if !safe_name(name) || content.len() > 1024 * 1024 {
                    return Err("Invalid generated filename or oversized content.".into());
                }
            }
        }
    }
    let mut files = vec![];
    for spec in specs {
        files.push(match spec {
            crate::response_channels::FileSpec::Existing { artifact_id } => lookup(artifact_id)?,
            crate::response_channels::FileSpec::Generated {
                name,
                content,
                media_type,
            } => {
                let dir = storage.join(crate::pipelines::new_id());
                std::fs::create_dir_all(&dir)
                    .map_err(|_| "Could not create attachment storage.")?;
                let path = dir.join(name);
                std::fs::write(&path, content)
                    .map_err(|_| "Could not save generated attachment.")?;
                register(&path, true, media_type.clone())?
            }
        });
    }
    Ok(files)
}
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 240
        && !name.contains(['/', '\\', ':', '\0'])
        && !name.ends_with(['.', ' '])
        && !name
            .chars()
            .any(|c| c.is_control() || "<>\"|?*".contains(c))
        && !matches!(name, "." | "..")
        && ![
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ]
        .contains(&name.split('.').next().unwrap_or("").to_uppercase().as_str())
}
pub fn open(id: &str) -> Result<(), String> {
    let file = lookup(id)?;
    if !file.path.is_file() {
        return Err("This attachment is no longer available.".into());
    }
    let extension = file
        .path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    if [
        "exe", "com", "bat", "cmd", "ps1", "vbs", "js", "msi", "lnk", "url", "scr", "hta", "reg",
    ]
    .contains(&extension.as_str())
        && !rfd::MessageDialog::new()
            .set_title("Open executable attachment?")
            .set_description("This attachment can run commands or change your computer.")
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            .eq(&rfd::MessageDialogResult::Yes)
    {
        return Ok(());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let path: Vec<u16> = file.path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            use windows::Win32::UI::Shell::ShellExecuteW;
            let result = ShellExecuteW(
                None,
                windows::core::w!("open"),
                windows::core::PCWSTR(path.as_ptr()),
                None,
                None,
                windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
            );
            if result.0 as isize <= 32 {
                return Err("Windows could not open this attachment.".into());
            }
        }
    }
    Ok(())
}
pub fn save_as(id: &str) -> Result<(), String> {
    let file = lookup(id)?;
    if let Some(destination) = rfd::FileDialog::new().set_file_name(&file.name).save_file() {
        std::fs::copy(file.path, destination).map_err(|_| "Could not save attachment.")?;
    }
    Ok(())
}
pub fn show_in_folder(id: &str) -> Result<(), String> {
    let file = lookup(id)?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer.exe")
            .arg(format!("/select,{}", file.path.display()))
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|_| "Could not show attachment folder.")?;
    }
    Ok(())
}
pub fn clear() -> Result<(), String> {
    let path = root();
    if path.exists() {
        let actual = path
            .canonicalize()
            .map_err(|_| "Could not resolve attachment storage.")?;
        let parent = crate::settings::settings_path()
            .parent()
            .ok_or("Invalid settings directory.")?
            .canonicalize()
            .map_err(|_| "Could not resolve settings directory.")?;
        if actual.parent() != Some(parent.as_path()) {
            return Err("Attachment storage is outside Echo's data directory.".into());
        }
        std::fs::remove_dir_all(actual).map_err(|_| "Could not clear attachment storage.")?;
    }
    REGISTRY.lock().unwrap().retain(|_, file| !file.generated);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_names_cannot_escape_storage() {
        for name in [
            "../a", "a/b", "a\\b", "C:thing", "NUL.txt", "a.", ".", "a\0b",
        ] {
            assert!(!safe_name(name));
        }
        assert!(safe_name("report.md"));
    }
    #[test]
    fn unknown_ids_cannot_be_opened() {
        assert!(lookup("unregistered").is_err());
    }
    #[test]
    fn generated_files_are_isolated_and_existing_files_remain_unchanged() {
        use crate::response_channels::FileSpec;
        let dir =
            std::env::temp_dir().join(format!("echo-artifacts-{}", crate::pipelines::new_id()));
        std::fs::create_dir(&dir).unwrap();
        let original = dir.join("report.md");
        std::fs::write(&original, "Original").unwrap();
        let file = register(&original, false, "text/markdown".into()).unwrap();
        let outputs = dir.join("outputs");
        let specs = [
            FileSpec::Generated {
                name: "report.md".into(),
                content: "Generated".into(),
                media_type: "text/markdown".into(),
            },
            FileSpec::Existing {
                artifact_id: file.id,
            },
        ];
        let first = materialize_at(&specs, &outputs).unwrap();
        let second = materialize_at(&specs, &outputs).unwrap();
        assert_ne!(first[0].path, second[0].path);
        assert_eq!(std::fs::read_to_string(&original).unwrap(), "Original");
        assert_eq!(first[1].path, original.canonicalize().unwrap());
        let invalid = [
            FileSpec::Generated {
                name: "valid.md".into(),
                content: "unused".into(),
                media_type: String::new(),
            },
            FileSpec::Generated {
                name: "../invalid".into(),
                content: String::new(),
                media_type: String::new(),
            },
        ];
        let unused = dir.join("unused");
        assert!(materialize_at(&invalid, &unused).is_err());
        assert!(!unused.exists());
        for file in [first[0].path.clone(), second[0].path.clone()] {
            std::fs::remove_file(&file).unwrap();
            std::fs::remove_dir(file.parent().unwrap()).unwrap();
        }
        std::fs::remove_dir(outputs).unwrap();
        std::fs::remove_file(original).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
