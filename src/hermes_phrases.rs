//! Optional, pre-generated acknowledgements. Playback never loads a model.
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

pub const GROUPS: &[(&str, &[&str])] = &[
    (
        "thinking",
        &[
            "Let me think about that.",
            "I'm thinking about this.",
            "Give me a moment to work that out.",
            "Let me consider that.",
            "I'll think it through.",
        ],
    ),
    (
        "checking",
        &[
            "Let me check that.",
            "I'll take a look.",
            "Let me verify that first.",
            "I'll check the details.",
            "All right, let me check.",
        ],
    ),
    (
        "researching",
        &[
            "I'll look into that.",
            "Let me find the information.",
            "I'll check the sources.",
            "Let me do a little research.",
            "I'll see what I can find.",
        ],
    ),
    (
        "working",
        &[
            "I'm working on it.",
            "I'll get started.",
            "All right, I'll handle that.",
            "I'll work through the steps.",
            "I've started working on that.",
        ],
    ),
    (
        "long_task",
        &[
            "This may take a little while.",
            "I'll keep working in the background.",
            "This needs a few steps.",
            "I'll let you know when there's an update.",
            "You can keep talking while I work.",
        ],
    ),
    (
        "queued",
        &[
            "That's in the queue.",
            "I'll get to that next.",
            "I've queued that up.",
            "That will start when the agent is ready.",
            "I've saved that for the next turn.",
        ],
    ),
    (
        "instructions_queued",
        &[
            "I've queued your instructions.",
            "The agent will see that at its next checkpoint.",
            "I've saved that correction for the agent.",
            "Those directions are queued for the worker.",
            "I've passed that to the task's instruction queue.",
        ],
    ),
    (
        "question",
        &[
            "I need one detail before I continue.",
            "Let me clarify one thing.",
            "I have a quick question.",
            "I need a little more information.",
            "There's one choice I need you to make.",
        ],
    ),
    (
        "approval",
        &[
            "This step needs your approval.",
            "Please review the access request.",
            "I'm waiting for your permission for this step.",
            "There's an approval request in the task tab.",
        ],
    ),
    (
        "completed",
        &[
            "That's finished.",
            "The task is complete.",
            "I've finished that work.",
            "Your results are ready.",
        ],
    ),
    (
        "failed",
        &[
            "The task ran into a problem.",
            "That step didn't finish successfully.",
            "There's an error to review.",
            "I couldn't complete that task.",
        ],
    ),
    (
        "disconnected",
        &[
            "The connection has dropped.",
            "I can't reach the backend right now.",
            "The assistant is disconnected.",
            "I need the connection restored before I continue.",
        ],
    ),
    (
        "reconnecting",
        &[
            "I'm reconnecting.",
            "I'll try to restore the connection.",
            "I'm checking the connection again.",
            "Let me reconnect to the backend.",
        ],
    ),
    (
        "model_loading",
        &[
            "The model is still loading.",
            "I'm waiting for the model to be ready.",
            "The model isn't ready yet.",
            "The model is warming up.",
        ],
    ),
    (
        "speech_loading",
        &[
            "The voice model is loading.",
            "Speech will be ready shortly.",
            "I'm preparing the voice model.",
            "The voice is still warming up.",
        ],
    ),
    (
        "listening",
        &[
            "I'm listening.",
            "Go ahead.",
            "What would you like to do?",
            "I'm ready to hear you.",
        ],
    ),
    (
        "transcribing",
        &[
            "I'm finishing the transcription.",
            "I'm processing what you said.",
            "Let me finish capturing that.",
            "I'm waiting for the last words.",
        ],
    ),
    (
        "paused",
        &[
            "The task is paused.",
            "I've paused at this step.",
            "Work is paused for now.",
            "That task is waiting to resume.",
        ],
    ),
    (
        "stopped",
        &[
            "The task has stopped.",
            "That task is cancelled.",
            "The worker has stopped.",
            "I've stopped that work.",
        ],
    ),
    (
        "switching",
        &[
            "Here's the latest from this agent.",
            "Let's look at this task.",
            "I've opened that conversation.",
            "Here's where this task stands.",
        ],
    ),
    (
        "resuming",
        &[
            "The task is resuming.",
            "I'll continue from here.",
            "The worker is starting again.",
            "Work is continuing now.",
        ],
    ),
    (
        "saving",
        &[
            "I'm saving the results.",
            "The results are being saved.",
            "I'm writing out the finished work.",
            "I'm saving those changes.",
        ],
    ),
    (
        "files_ready",
        &[
            "The generated files are ready.",
            "You can open the files now.",
            "Your files are available.",
            "The saved files are ready to review.",
        ],
    ),
    (
        "review_ready",
        &[
            "The work is ready for review.",
            "Please take a look at the results.",
            "There's a finished change to review.",
            "I've reached the review step.",
        ],
    ),
];
static RECENT: LazyLock<Mutex<VecDeque<usize>>> = LazyLock::new(|| Mutex::new(VecDeque::new()));

pub fn directory(voice: &Path) -> Result<PathBuf, String> {
    Ok(crate::hermes_runtime::root()
        .join("phrase-cache")
        .join(crate::assistant_voice::phrase_fingerprint(voice)?))
}
fn file(directory: &Path, text: &str) -> PathBuf {
    directory.join(format!("{:x}.wav", Sha256::digest(text.as_bytes())))
}

pub fn prepare(
    voice: &Path,
    stopped: impl Fn() -> bool,
    idle: impl Fn() -> bool,
    progress: impl Fn(usize, usize),
) -> Result<bool, String> {
    if stopped() {
        return Ok(false);
    }
    let directory = directory(voice)?;
    std::fs::create_dir_all(&directory).map_err(|_| "Could not prepare the voice phrase cache.")?;
    let store = crate::sayings::Store::for_voice(voice);
    let sayings = store.list()?;
    let total = sayings.len();
    for (index, saying) in sayings.iter().enumerate() {
        if !generate_one(voice, &directory, &store, saying, false, &stopped, &idle)? {
            return Ok(false);
        }
        progress(index + 1, total);
    }
    Ok(true)
}

pub fn play(category: &str, voice: &Path, generation: u64) -> Result<Option<String>, String> {
    let directory = directory(voice)?;
    let store = crate::sayings::Store::for_voice(voice);
    let sayings = store.list()?;
    let mut pool = Vec::new();
    let recent = RECENT.lock().unwrap();
    for (offset, saying) in sayings.into_iter().enumerate() {
        let path = audio_path(&directory, &saying);
        if saying.category == category && !recent.contains(&offset) && path.is_file() {
            pool.push((offset, saying.text, path));
        }
    }
    drop(recent);
    if pool.is_empty() {
        return Ok(None);
    }
    let mut random = [0u8; 1];
    let _ = getrandom::fill(&mut random);
    let (id, text, path) = &pool[usize::from(random[0]) % pool.len()];
    if !crate::assistant_voice::play_cached_phrase(path, generation)? {
        return Ok(None);
    }
    let mut recent = RECENT.lock().unwrap();
    recent.push_back(*id);
    while recent.len() > 3 {
        recent.pop_front();
    }
    Ok(Some(text.clone()))
}

pub fn audio_path(directory: &Path, saying: &crate::sayings::Saying) -> PathBuf {
    file(directory, &saying.text)
}

pub fn generate_one(
    voice: &Path,
    directory: &Path,
    store: &crate::sayings::Store,
    saying: &crate::sayings::Saying,
    force: bool,
    stopped: &impl Fn() -> bool,
    idle: &impl Fn() -> bool,
) -> Result<bool, String> {
    if !force && audio_path(directory, saying).is_file() {
        return Ok(true);
    }
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let destination = file(directory, &saying.text);
    let staged = directory.join(format!("{}.wav", crate::pipelines::new_id()));
    let result = (|| {
        loop {
            if stopped() {
                return Ok(false);
            }
            if idle()
                && crate::assistant_voice::render_cached_phrase(
                    &saying.text,
                    voice,
                    &staged,
                    || stopped() || !idle(),
                )?
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if stopped() {
            return Ok(false);
        }
        if store.get(&saying.id)?.text != saying.text {
            return Err("The saying changed during generation.".into());
        }
        std::fs::rename(&staged, &destination).map_err(|e| e.to_string())?;
        Ok(true)
    })();
    let _ = std::fs::remove_file(staged);
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn edited_words_do_not_reuse_old_audio_and_cancel_keeps_existing_clips() {
        let root = std::env::temp_dir().join(crate::pipelines::new_id());
        std::fs::create_dir_all(&root).unwrap();
        let voice = std::path::Path::new("test-voice.safetensors");
        let store = crate::sayings::Store::at(&root.join("words"), voice);
        let before = store.get("thinking-0").unwrap();
        let old = super::audio_path(&root, &before);
        std::fs::write(&old, b"keep existing audio").unwrap();
        store
            .edit(&before.id, "A new version of this saying.")
            .unwrap();
        let after = store.get(&before.id).unwrap();
        let new = super::audio_path(&root, &after);
        assert_ne!(old, new);
        assert!(!new.exists());
        assert!(
            !super::generate_one(voice, &root, &store, &after, true, &|| true, &|| true).unwrap()
        );
        assert_eq!(std::fs::read(old).unwrap(), b"keep existing audio");
        assert!(!new.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "installed Pocket TTS; isolated regeneration, no playback or user cache writes"]
    fn selected_saying_force_regenerates_existing_audio_atomically() {
        let root = std::env::temp_dir().join(crate::pipelines::new_id());
        std::fs::create_dir_all(&root).unwrap();
        let voice = std::path::Path::new("");
        let store = crate::sayings::Store::at(&root.join("words"), voice);
        let saying = store.get("thinking-0").unwrap();
        let path = super::audio_path(&root, &saying);
        std::fs::write(&path, b"old audio placeholder").unwrap();
        assert!(
            super::generate_one(voice, &root, &store, &saying, false, &|| false, &|| true).unwrap()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"old audio placeholder");
        assert!(
            super::generate_one(voice, &root, &store, &saying, true, &|| false, &|| true).unwrap()
        );
        let reader = hound::WavReader::open(&path).unwrap();
        assert!(reader.duration() > 0);
        assert_eq!(reader.spec().channels, 1);
        drop(reader);
        let valid = std::fs::read(&path).unwrap();
        assert!(
            !super::generate_one(voice, &root, &store, &saying, true, &|| true, &|| true).unwrap()
        );
        assert_eq!(std::fs::read(&path).unwrap(), valid);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "prepares installed Pocket TTS phrase cache with synthetic text; no audio output"]
    fn installed_phrase_cache_is_complete_and_reusable() {
        let voice = std::path::Path::new("");
        assert!(super::prepare(
            voice,
            || false,
            || true,
            |done, total| {
                if done % 10 == 0 || done == total {
                    eprintln!("Cached phrases: {done}/{total}");
                }
            }
        )
        .unwrap());
        let started = std::time::Instant::now();
        assert!(super::prepare(voice, || false, || true, |_, _| {}).unwrap());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "Existing cache regenerated instead of resuming"
        );
        let directory = super::directory(voice).unwrap();
        for (_, phrases) in super::GROUPS {
            for text in *phrases {
                let reader = hound::WavReader::open(super::file(&directory, text)).unwrap();
                assert!(reader.duration() > 0);
                assert_eq!(reader.spec().channels, 1);
            }
        }
    }

    #[test]
    fn cancelled_preparation_is_not_reported_as_ready_or_loads_a_voice() {
        let result = super::prepare(
            std::path::Path::new("missing-voice.safetensors"),
            || true,
            || true,
            |_, _| panic!("cancelled cache made progress"),
        );
        assert!(!result.unwrap());
    }

    #[test]
    fn catalogue_has_104_unique_phrases_for_24_situations() {
        let phrases = super::GROUPS
            .iter()
            .flat_map(|(_, phrases)| phrases.iter())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(super::GROUPS.len(), 24);
        assert_eq!(phrases.len(), 104);
    }
}
