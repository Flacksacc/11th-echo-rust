mod rules;
use crate::settings::PostProcessingSettings;
use futures_util::StreamExt;
pub use rules::validate;
use sha2::{Digest, Sha256};
use sherpa_onnx::{OfflinePunctuation, OfflinePunctuationConfig, OfflinePunctuationModelConfig};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

const MODEL: &str = "model.int8.onnx";
const URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/punctuation-models/sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8.tar.bz2";
const ARCHIVE_HASH: &str = "c0d5aa5f8eeb686032345e180bedf39319dc2e0556781c6264bcadba8328a6e1";
const ARCHIVE_SIZE: u64 = 64_717_756;
const MODEL_HASH: &str = "65a3fb9f5ad7bfb96bf69e0dc4481df97f6ee60513c1d94ce981ba6effd524b1";
static PUNCTUATOR: Mutex<Option<OfflinePunctuation>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Recording/finalization and explicit model installation are mutually exclusive.
pub struct ActivityGuard;
impl ActivityGuard {
    pub fn acquire() -> Option<Self> {
        ACTIVE
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| Self)
    }
}
impl Drop for ActivityGuard {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::SeqCst);
    }
}

pub struct ProcessedText {
    pub text: String,
    pub warning: Option<String>,
}

/// Called on the formatting worker after the provider has drained on stop.
pub fn process(input: &str, settings: &PostProcessingSettings) -> ProcessedText {
    process_with(input, settings, punctuate)
}

fn process_with(
    input: &str,
    s: &PostProcessingSettings,
    infer: impl Fn(&str) -> Result<String, String>,
) -> ProcessedText {
    if !s.enabled || input.trim().is_empty() {
        return ProcessedText {
            text: input.to_string(),
            warning: None,
        };
    }
    if let Err(warning) = validate(s) {
        return ProcessedText {
            text: input.into(),
            warning: Some(warning),
        };
    }
    // Keep the source separately for failure recovery. Successful rebuilds
    // strip ASR sentence marks FIRST, then apply rules, then infer punctuation.
    let source = if s.punctuation {
        rules::strip_sentence_punctuation(input, s)
    } else {
        input.into()
    };
    let atoms = rules::apply(&source, s);
    let fallback = atoms
        .iter()
        .map(|a| a.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if !s.punctuation {
        return ProcessedText {
            text: if s.capitalization {
                capitalize_existing(&atoms)
            } else {
                fallback
            },
            warning: None,
        };
    }
    let mut clean = Vec::new();
    let mut projection = Vec::new();
    for atom in &atoms {
        let text = atom.content.as_str();
        if text.is_empty() {
            continue;
        }
        // Only the inference view substitutes protected expressions. The model
        // never owns their text; we project validated boundary marks back onto
        // the originals, so tokenizers cannot corrupt emails or custom names.
        projection.push(if atom.protected {
            "something".to_string()
        } else {
            text.to_lowercase()
        });
        clean.push(atom.clone());
    }
    let mut marks = vec![String::new(); clean.len()];
    // Bounded windows with context overlap. Each boundary has one owner.
    for start in (0..clean.len()).step_by(96) {
        let end = (start + 96).min(clean.len());
        let left = start.saturating_sub(16);
        let right = (end + 16).min(clean.len());
        let output = match infer(&projection[left..right].join(" "))
            .and_then(|out| boundary_marks(&projection[left..right], &out))
        {
            Ok(output) => output,
            Err(warning) => {
                return ProcessedText {
                    text: rules::apply(input, s)
                        .iter()
                        .map(|a| a.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                    warning: Some(warning),
                }
            }
        };
        marks[start..end].clone_from_slice(&output[(start - left)..(end - left)]);
    }
    let mut sentence_start = true;
    let mut text = String::new();
    for (atom, mark) in clean.iter().zip(marks) {
        if !text.is_empty() {
            text.push(' ');
        }
        let mut word = atom.content.clone();
        if s.capitalization && !atom.case_protected {
            word = case_word(&word, sentence_start);
        }
        text.push_str(&word);
        let mark = mark
            .chars()
            .filter(|c| match c {
                ',' => s.commas,
                '.' => s.periods,
                '?' => s.question_marks,
                _ => false,
            })
            .collect::<String>();
        sentence_start = mark.ends_with(['.', '?']);
        if !(word.ends_with('.') && mark == ".") {
            text.push_str(&mark);
        }
    }
    ProcessedText {
        text,
        warning: None,
    }
}

fn case_word(word: &str, start: bool) -> String {
    if matches!(
        word.to_lowercase().as_str(),
        "i" | "i'm" | "i'll" | "i've" | "i'd" | "i’m" | "i’ll" | "i’ve" | "i’d"
    ) {
        return "I".to_owned() + &word[1..];
    }
    if !start {
        return word.into();
    }
    let mut chars = word.chars();
    chars.next().map_or(String::new(), |c| {
        c.to_uppercase().collect::<String>() + chars.as_str()
    })
}
fn capitalize_existing(atoms: &[rules::Atom]) -> String {
    let mut start = true;
    atoms
        .iter()
        .map(|a| {
            let w = if a.case_protected {
                a.text.clone()
            } else {
                case_word(&a.text, start)
            };
            start = a.text.ends_with(['.', '?', '!']);
            w
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract additions only; never accept changed, lost, duplicated model words.
fn boundary_marks(words: &[String], output: &str) -> Result<Vec<String>, String> {
    let mut stream = output
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .peekable();
    let mut result = Vec::new();
    for word in words {
        for expected in word
            .chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(char::to_lowercase)
        {
            if stream.next() != Some(expected) {
                return Err(
                    "Punctuation output changed words; original punctuation retained.".into(),
                );
            }
        }
        let mut mark = String::new();
        while let Some(c) = stream.peek().copied() {
            let mapped = match c {
                ',' | '，' => ',',
                '.' | '。' => '.',
                '?' | '？' => '?',
                '!' | '！' => '.',
                _ => break,
            };
            stream.next();
            mark.push(mapped);
        }
        result.push(mark);
    }
    if stream.next().is_some() {
        return Err("Punctuation output did not align; original punctuation retained.".into());
    }
    Ok(result)
}

fn punctuation_path() -> PathBuf {
    dirs_next::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("11th_echo/models/punctuation")
        .join(MODEL)
}
pub fn punctuation_model_available() -> bool {
    punctuation_path()
        .metadata()
        .is_ok_and(|m| m.len() > 1_000_000)
}
fn load(path: &Path) -> Result<OfflinePunctuation, String> {
    if !path.is_file() {
        return Err(
            "Punctuation model is missing. Enable post-processing in Settings to install it."
                .into(),
        );
    }
    if hash_file(path)? != MODEL_HASH {
        return Err("Installed punctuation model failed verification. Re-enable post-processing to repair it.".into());
    }
    OfflinePunctuation::create(&OfflinePunctuationConfig {
        model: OfflinePunctuationModelConfig {
            ct_transformer: Some(path.to_string_lossy().into_owned()),
            num_threads: 1,
            provider: Some("cpu".into()),
            debug: false,
        },
    })
    .ok_or_else(|| {
        "Punctuation model could not load. Re-enable post-processing to repair it.".into()
    })
}
fn punctuate(text: &str) -> Result<String, String> {
    let mut model = PUNCTUATOR
        .lock()
        .map_err(|_| "Punctuation worker needs a restart.")?;
    if model.is_none() {
        *model = Some(load(&punctuation_path())?);
    }
    model
        .as_ref()
        .unwrap()
        .add_punctuation(text)
        .ok_or_else(|| "Punctuation inference failed; original punctuation retained.".into())
}
pub fn preload() -> Result<(), String> {
    punctuate("hello how are you").map(|_| ())
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|_| "Cannot open model archive.")?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|_| "Cannot read model archive.")?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn extract_verified(archive: &Path, staged: &Path) -> Result<(), String> {
    if archive
        .metadata()
        .map_err(|_| "Missing model archive.")?
        .len()
        != ARCHIVE_SIZE
        || hash_file(archive)? != ARCHIVE_HASH
    {
        return Err("Model archive failed SHA-256 verification.".into());
    }
    let source = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut tar = tar::Archive::new(bzip2::read::BzDecoder::new(source));
    let mut found = false;
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?;
        if path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err("Unsafe path in model archive.".into());
        }
        if path.file_name().and_then(|n| n.to_str()) != Some(MODEL) {
            continue;
        }
        if found || !entry.header().entry_type().is_file() || entry.size() > 512 * 1024 * 1024 {
            return Err("Invalid punctuation model archive entry.".into());
        }
        let mut out = std::fs::File::create(staged).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        out.sync_all().map_err(|e| e.to_string())?;
        found = true;
    }
    if !found {
        return Err("Archive has no punctuation model.".into());
    }
    Ok(())
}

pub async fn download_punctuation_model<F>(progress: F) -> Result<(), String>
where
    F: Fn(f32, String) + Send + Sync + 'static,
{
    download_to(punctuation_path(), progress).await
}

async fn download_to<F>(destination: PathBuf, progress: F) -> Result<(), String>
where
    F: Fn(f32, String) + Send + Sync + 'static,
{
    let directory = destination.parent().unwrap();
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(|e| e.to_string())?;
    let archive = directory.join("punctuation.tar.bz2.part");
    let staged = directory.join("model.installing.onnx");
    // Reuse a previous complete download only after verification. No network
    // activity is started here except following explicit modal acceptance.
    progress(0.01, "Checking previously downloaded files…".into());
    let cached = archive.clone();
    let reusable = tokio::task::spawn_blocking(move || {
        cached.metadata().is_ok_and(|m| m.len() == ARCHIVE_SIZE)
            && hash_file(&cached).is_ok_and(|h| h == ARCHIVE_HASH)
    })
    .await
    .unwrap_or(false);
    if !reusable {
        let client = reqwest::Client::builder()
            .https_only(true)
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(|e| e.to_string())?;
        let response = client
            .get(URL)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?;
        if response.content_length().is_some_and(|n| n != ARCHIVE_SIZE) {
            return Err("Model server returned an unexpected file size.".into());
        }
        let mut file = tokio::fs::File::create(&archive)
            .await
            .map_err(|e| e.to_string())?;
        let mut stream = response.bytes_stream();
        let mut received = 0u64;
        let mut last = Instant::now() - Duration::from_secs(1);
        while let Some(chunk) = tokio::time::timeout(Duration::from_secs(60), stream.next())
            .await
            .map_err(|_| "Model download stalled; please retry.")?
        {
            let chunk = chunk.map_err(|e| e.to_string())?;
            received += chunk.len() as u64;
            if received > ARCHIVE_SIZE {
                return Err("Model download exceeds expected size.".into());
            }
            file.write_all(&chunk).await.map_err(|e| e.to_string())?;
            if last.elapsed() > Duration::from_millis(100) {
                progress(
                    received as f32 / ARCHIVE_SIZE as f32 * 0.82,
                    format!("Downloading… {}%", received * 100 / ARCHIVE_SIZE),
                );
                last = Instant::now();
            }
        }
        file.flush().await.map_err(|e| e.to_string())?;
        drop(file);
        if received != ARCHIVE_SIZE {
            return Err("Model download was incomplete; please retry.".into());
        }
    }
    let backup = directory.join("model.previous.onnx");
    tokio::task::spawn_blocking(move || {
        progress(0.85, "Verifying and unpacking model…".into());
        extract_verified(&archive, &staged)?;
        progress(0.93, "Loading model and testing punctuation…".into());
        let model = load(&staged)?;
        let probe = model
            .add_punctuation("hello how are you")
            .ok_or("Model inference test failed.")?;
        boundary_marks(
            &["hello".into(), "how".into(), "are".into(), "you".into()],
            &probe,
        )?;
        let mut current = PUNCTUATOR
            .lock()
            .map_err(|_| "Punctuation worker needs a restart.")?;
        if backup.exists() {
            std::fs::remove_file(&backup).map_err(|e| e.to_string())?;
        }
        let had_old = destination.exists();
        if had_old {
            std::fs::rename(&destination, &backup).map_err(|e| e.to_string())?;
        }
        if let Err(e) = std::fs::rename(&staged, &destination) {
            if had_old {
                let _ = std::fs::rename(&backup, &destination);
            }
            return Err(e.to_string());
        }
        *current = Some(model);
        if had_old {
            let _ = std::fs::remove_file(&backup);
        }
        let _ = std::fs::remove_file(&archive);
        progress(1.0, "Installed, tested, and ready to use.".into());
        Ok(())
    })
    .await
    .map_err(|_| "Model installation worker failed.".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn punctuation_applies_only_boundary_marks() {
        let s = PostProcessingSettings::default();
        let r = process_with(
            "email john dot smith at example dot com please",
            &s,
            |input| {
                assert_eq!(input, "email something please");
                Ok("email something, please.".into())
            },
        );
        assert_eq!(r.text, "Email john.smith@example.com, please.");
        assert!(r.warning.is_none());
    }

    #[test]
    fn multiword_spoken_email_is_one_protected_span() {
        let s = PostProcessingSettings::default();
        let r = process_with(
            "please email mary jane watson at research lab dot example dot com tomorrow",
            &s,
            |input| {
                assert_eq!(input, "please email something tomorrow");
                Ok("please email something, tomorrow.".into())
            },
        );
        assert_eq!(
            r.text,
            "Please email maryjanewatson@researchlab.example.com, tomorrow."
        );
        assert!(r.warning.is_none());
    }
    #[test]
    fn failure_keeps_original_punctuation_and_successful_rules() {
        let r = process_with(
            "Pay twelve dollars. Thanks!",
            &PostProcessingSettings::default(),
            |_| Err("missing model".into()),
        );
        assert_eq!(r.text, "Pay $12. Thanks!");
        assert!(r.warning.is_some());
    }
    #[test]
    fn punctuation_and_case_switches_work() {
        let s = PostProcessingSettings {
            commas: false,
            question_marks: false,
            capitalization: false,
            ..Default::default()
        };
        assert_eq!(
            process_with("hello. how are you?", &s, |input| {
                assert_eq!(input, "hello how are you");
                Ok("hello, how are you?".into())
            })
            .text,
            "hello how are you"
        );
        let s = PostProcessingSettings {
            enabled: false,
            ..s
        };
        assert_eq!(process_with(" hello! ", &s, |_| panic!()).text, " hello! ");
    }
    #[test]
    fn checksum_is_full_sha256() {
        assert_eq!(ARCHIVE_HASH.len(), 64);
    }
    #[test]
    fn model_cannot_change_words() {
        assert!(boundary_marks(&["hello".into()], "goodbye.").is_err());
    }

    #[test]
    fn stripping_preserves_internal_symbols_and_user_literals() {
        let s = PostProcessingSettings {
            custom_replacements: vec!["brand => Acme, Inc.".into()],
            ..Default::default()
        };
        let r = process_with(
            "hello,world! U.S. 3.5 $12.05 john@example.com brand",
            &s,
            |input| {
                assert_eq!(
                    input,
                    "hello world something something something something something"
                );
                Ok(format!("{input}."))
            },
        );
        assert_eq!(
            r.text,
            "Hello world U.S. 3.5 $12.05 john@example.com Acme, Inc."
        );
        assert!(r.warning.is_none());
    }

    #[test]
    fn stripping_precedes_written_rules_even_across_wrong_asr_breaks() {
        let r = process_with(
            "pay twenty. Five dollars at three p.m.!",
            &PostProcessingSettings::default(),
            |input| {
                assert_eq!(input, "pay something at something");
                Ok(format!("{input}."))
            },
        );
        assert_eq!(r.text, "Pay $25 at 3:00 PM.");
        assert!(r.warning.is_none());
    }

    #[test]
    fn each_punctuation_filter_is_independent() {
        for (commas, periods, questions, expected) in [
            (true, true, true, "Hello, world. How are you?"),
            (false, true, true, "Hello world. How are you?"),
            (true, false, true, "Hello, world how are you?"),
            (true, true, false, "Hello, world. How are you"),
        ] {
            let s = PostProcessingSettings {
                commas,
                periods,
                question_marks: questions,
                ..Default::default()
            };
            assert_eq!(
                process_with("hello world how are you", &s, |_| Ok(
                    "hello, world. how are you?".into()
                ))
                .text,
                expected
            );
        }
    }

    #[test]
    fn punctuation_bypass_does_not_require_a_model() {
        let s = PostProcessingSettings {
            punctuation: false,
            ..Default::default()
        };
        assert_eq!(
            process_with("pay twelve dollars. i agree!", &s, |_| panic!(
                "must not load a model"
            ))
            .text,
            "Pay $12. I agree!"
        );
        let s = PostProcessingSettings {
            capitalization: false,
            ..s
        };
        assert_eq!(
            process_with("pay twelve dollars. i agree!", &s, |_| panic!()).text,
            "pay $12. i agree!"
        );
    }

    #[test]
    fn case_is_independent_of_written_category_toggles() {
        let s = PostProcessingSettings {
            whole_numbers: false,
            ..Default::default()
        };
        assert_eq!(
            process_with("three items", &s, |input| Ok(format!("{input}."))).text,
            "Three items."
        );
        assert_eq!(
            process_with("phone number zero zero seven", &s, |input| Ok(format!(
                "{input}."
            )))
            .text,
            "Phone number 007."
        );
        assert_eq!(
            process_with("hello i'm here", &s, |input| Ok(format!("{input}."))).text,
            "Hello I'm here."
        );
    }

    #[test]
    fn windows_have_single_boundary_ownership_without_word_loss() {
        let input = std::iter::repeat_n("ordinary", 350)
            .collect::<Vec<_>>()
            .join(" ");
        let calls = std::cell::Cell::new(0);
        let r = process_with(&input, &PostProcessingSettings::default(), |text| {
            calls.set(calls.get() + 1);
            assert!(text.split_whitespace().count() <= 128);
            Ok(format!("{text}."))
        });
        assert_eq!(calls.get(), 4);
        assert_eq!(r.text.split_whitespace().count(), 350);
        assert_eq!(r.text.matches('.').count(), 1);
        assert!(r.warning.is_none());
    }

    #[test]
    fn activity_guard_serializes_installation_and_recording() {
        let first = ActivityGuard::acquire().unwrap();
        assert!(ActivityGuard::acquire().is_none());
        drop(first);
        assert!(ActivityGuard::acquire().is_some());
    }

    #[test]
    fn a_truncated_archive_cannot_overwrite_the_model() {
        let dir =
            std::env::temp_dir().join(format!("echo-corrupt-archive-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("truncated.tar.bz2");
        let model = dir.join(MODEL);
        std::fs::write(&archive, b"truncated").unwrap();
        std::fs::write(&model, b"existing model").unwrap();
        assert!(extract_verified(&archive, &model).is_err());
        assert_eq!(std::fs::read(&model).unwrap(), b"existing model");
        std::fs::remove_file(archive).unwrap();
        std::fs::remove_file(model).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
    #[test]
    #[ignore = "requires locally downloaded verified archive; runs real CPU inference"]
    fn native_model_smoke() {
        let archive =
            std::env::var_os("ECHO_PUNCTUATION_TEST_ARCHIVE").expect("set test archive path");
        let dir =
            std::env::temp_dir().join(format!("echo-punctuation-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let model_path = dir.join(MODEL);
        extract_verified(Path::new(&archive), &model_path).unwrap();
        eprintln!("Verified model SHA-256 {}", hash_file(&model_path).unwrap());
        let start = Instant::now();
        let model = load(&model_path).unwrap();
        eprintln!("CPU load {:?}", start.elapsed());
        for input in [
            "how are you i am fine thank you",
            "email john dot smith at example dot com",
            "pay twelve dollars and five cents",
            "i went to the store did you need anything",
        ] {
            let result = process_with(input, &PostProcessingSettings::default(), |s| {
                model.add_punctuation(s).ok_or("inference failed".into())
            });
            eprintln!("{input} -> {} {:?}", result.text, result.warning);
            assert!(result.warning.is_none());
            assert!(!result.text.is_empty());
        }
        drop(model);
        std::fs::remove_file(model_path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    #[ignore = "requires cached verified archive; tests installation in a temporary directory without downloading"]
    fn cached_model_installation_smoke() {
        let source =
            std::env::var_os("ECHO_PUNCTUATION_TEST_ARCHIVE").expect("set test archive path");
        let dir = std::env::temp_dir().join(format!("echo-install-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(source, dir.join("punctuation.tar.bz2.part")).unwrap();
        let destination = dir.join(MODEL);
        std::fs::write(&destination, b"previous broken model").unwrap();
        let updates = std::sync::Arc::new(Mutex::new(Vec::new()));
        let captured = updates.clone();
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(download_to(destination.clone(), move |progress, status| {
                eprintln!("{:.0}% {status}", progress * 100.0);
                captured.lock().unwrap().push(progress);
            }))
            .unwrap();
        assert_eq!(hash_file(&destination).unwrap(), MODEL_HASH);
        assert!(!dir.join("punctuation.tar.bz2.part").exists());
        assert!(!dir.join("model.previous.onnx").exists());
        assert_eq!(updates.lock().unwrap().last(), Some(&1.0));
        *PUNCTUATOR.lock().unwrap() = None;
        std::fs::remove_file(destination).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
