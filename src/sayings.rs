//! Per-voice wording overrides for the built-in assistant sayings.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};

static WRITES: Mutex<()> = Mutex::new(());
#[derive(Default, Serialize, Deserialize)]
struct Override {
    text: String,
}
#[derive(Clone, Debug)]
pub struct Saying {
    pub id: String,
    pub category: String,
    pub text: String,
}
#[derive(Clone)]
pub struct Store {
    directory: PathBuf,
}
impl Store {
    pub fn for_voice(voice: &Path) -> Self {
        let root = std::env::var_os("ECHO_SAYINGS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::settings::settings_path().with_file_name("voice_sayings"));
        Self::at(&root, voice)
    }
    pub(crate) fn at(root: &Path, voice: &Path) -> Self {
        let key = voice.to_string_lossy().replace('\\', "/");
        #[cfg(windows)]
        let key = key.to_lowercase();
        Self {
            directory: root.join(format!("{:x}", Sha256::digest(key.as_bytes()))),
        }
    }
    pub fn revision(&self) -> String {
        let metadata = std::fs::metadata(self.directory.join("sayings.json")).ok();
        format!(
            "{:?}/{:?}",
            metadata.as_ref().map(|m| m.len()),
            metadata.and_then(|m| m.modified().ok())
        )
    }
    fn load(&self) -> Result<BTreeMap<String, Override>, String> {
        let path = self.directory.join("sayings.json");
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(262_145)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 262_144 {
            return Err("The sayings file is too large.".into());
        }
        let values: BTreeMap<String, Override> =
            serde_json::from_slice(&bytes).map_err(|_| "The sayings file is invalid.")?;
        for (id, value) in &values {
            default(id)?;
            validate_text(&value.text)?;
        }
        Ok(values)
    }
    fn save(&self, values: &BTreeMap<String, Override>) -> Result<(), String> {
        std::fs::create_dir_all(&self.directory).map_err(|e| e.to_string())?;
        let temp = self
            .directory
            .join(format!("{}.pending", crate::pipelines::new_id()));
        let result = (|| {
            std::fs::write(
                &temp,
                serde_json::to_vec(values).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            std::fs::rename(&temp, self.directory.join("sayings.json")).map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result
    }
    pub fn list(&self) -> Result<Vec<Saying>, String> {
        let values = self.load()?;
        Ok(crate::hermes_phrases::GROUPS
            .iter()
            .flat_map(|(category, texts)| {
                texts
                    .iter()
                    .enumerate()
                    .map(move |(index, text)| (*category, index, *text))
            })
            .map(|(category, index, text)| {
                let id = format!("{category}-{index}");
                let saved = values.get(&id);
                Saying {
                    id,
                    category: category.into(),
                    text: saved.map_or_else(|| text.into(), |v| v.text.clone()),
                }
            })
            .collect())
    }
    pub fn get(&self, id: &str) -> Result<Saying, String> {
        self.list()?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| "Select a saying.".into())
    }
    pub fn edit(&self, id: &str, text: &str) -> Result<(), String> {
        default(id)?;
        let text = validate_text(text)?;
        let _lock = WRITES.lock().unwrap();
        let mut values = self.load()?;
        if values.get(id).is_some_and(|v| v.text == text) {
            return Ok(());
        }
        values.insert(id.into(), Override { text });
        self.save(&values)
    }
    pub fn original_text(&self, id: &str) -> Result<String, String> {
        default(id).map(|(_, text)| text.to_owned())
    }
    #[cfg(test)]
    pub fn restore(&self, id: &str) -> Result<(), String> {
        let (_, text) = default(id)?;
        self.edit(id, text)
    }
}
fn default(id: &str) -> Result<(&'static str, &'static str), String> {
    for (category, texts) in crate::hermes_phrases::GROUPS {
        for (index, text) in texts.iter().enumerate() {
            if id == format!("{category}-{index}") {
                return Ok((category, text));
            }
        }
    }
    Err("Unknown saying.".into())
}
fn validate_text(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > 300 || text.chars().any(char::is_control) {
        return Err("Enter a saying of 1–300 characters on one line.".into());
    }
    Ok(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn per_voice_words_persist_and_other_voices_remain_unchanged() {
        let root = std::env::temp_dir().join(crate::pipelines::new_id());
        let first = Store::at(&root, Path::new("one.safetensors"));
        let second = Store::at(&root, Path::new("two.safetensors"));
        assert_eq!(first.list().unwrap().len(), 104);
        first.edit("thinking-0", "Let me work that out.").unwrap();
        let old = first.get("thinking-0").unwrap();
        assert_eq!(
            Store::at(&root, Path::new("one.safetensors"))
                .get(&old.id)
                .unwrap()
                .text,
            old.text
        );
        assert_ne!(second.get(&old.id).unwrap().text, old.text);
        first.edit(&old.id, "A different saying.").unwrap();
        let current = first.get(&old.id).unwrap();
        assert_ne!(old.text, current.text);
        first.restore(&old.id).unwrap();
        assert_eq!(
            first.get(&old.id).unwrap().text,
            default(&old.id).unwrap().1
        );
        assert!(first.edit("../outside", "invalid").is_err());
        assert!(first.edit(&old.id, "").is_err());
        assert!(first.edit(&old.id, "two\nlines").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
