mod rules;
use crate::settings::PostProcessingSettings;
use futures_util::StreamExt;
pub use rules::validate;
use sha2::{Digest, Sha256};
use sherpa_onnx::{OnlinePunctuation, OnlinePunctuationConfig, OnlinePunctuationModelConfig};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

/// Edge-Punct-Casing jointly predicts sentence marks and word casing. It is
/// the small, English-only Sherpa ONNX package; it replaces the former
/// multilingual CT-Transformer which predicted punctuation only.
const MODEL_DIRECTORY: &str = "edge-punct-casing";
const MODEL: &str = "model.int8.onnx";
const VOCABULARY: &str = "bpe.vocab";
const URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/punctuation-models/sherpa-onnx-online-punct-en-2024-08-06.tar.bz2";
const ARCHIVE_HASH: &str = "9f5e5a72c7d2829635bd074fce92b6bbd5b78da8a52e7ad8ed1be933f366b99d";
const ARCHIVE_SIZE: u64 = 30_667_839;
const MODEL_HASH: &str = "9d611f445fe4a46186080fe161be6059d87d72eb88d3a8cb00c1a06e83a6067e";
const MODEL_SIZE: u64 = 7_490_500;
const VOCABULARY_HASH: &str = "e118b7ad88c54db562517df49e1cffd4836d166c34fb190fd311d7f34eb238f5";
const VOCABULARY_SIZE: u64 = 149_430;
static PUNCTUATOR: Mutex<Option<OnlinePunctuation>> = Mutex::new(None);
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

#[derive(Clone, Debug)]
struct WordPrediction {
    punctuation: String,
    casing: String,
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
    let mut predictions = (0..clean.len())
        .map(|_| WordPrediction {
            punctuation: String::new(),
            casing: String::new(),
        })
        .collect::<Vec<_>>();
    // Bounded windows with context overlap. Each boundary has one owner.
    for start in (0..clean.len()).step_by(96) {
        let end = (start + 96).min(clean.len());
        let left = start.saturating_sub(16);
        let right = (end + 16).min(clean.len());
        let output = match infer(&projection[left..right].join(" "))
            .and_then(|out| word_predictions(&projection[left..right], &out))
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
        predictions[start..end].clone_from_slice(&output[(start - left)..(end - left)]);
    }
    let mut sentence_start = true;
    let mut text = String::new();
    for (atom, prediction) in clean.iter().zip(predictions) {
        if !text.is_empty() {
            text.push(' ');
        }
        let mut word = atom.content.clone();
        if s.capitalization && !atom.case_protected {
            word = apply_model_casing(&word, &prediction.casing);
            // A learned casing prediction should improve names and interior
            // words, but preserve the old guaranteed sentence-start and "I"
            // behavior if a model ever emits lowercase there.
            word = case_word(&word, sentence_start);
        }
        text.push_str(&word);
        let mark = prediction
            .punctuation
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

/// The model sees lowercase words, but returns their predicted casing. Keep
/// original letters unless the model asks to make one uppercase; this lets a
/// pre-capitalized name survive a mistaken lowercase prediction.
fn apply_model_casing(word: &str, predicted: &str) -> String {
    if word.chars().count() != predicted.chars().count() {
        return word.to_string();
    }
    word.chars()
        .zip(predicted.chars())
        .fold(String::new(), |mut result, (source, prediction)| {
            if prediction.is_uppercase() {
                result.extend(source.to_uppercase());
            } else {
                result.push(source);
            }
            result
        })
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
/// Validates that inference changed only case and boundary punctuation. This
/// keeps protected names, addresses, and written values owned by Echo.
fn word_predictions(words: &[String], output: &str) -> Result<Vec<WordPrediction>, String> {
    let mut stream = output.chars().filter(|c| !c.is_whitespace()).peekable();
    let mut result = Vec::new();
    for word in words {
        let mut casing = String::new();
        for expected in word.chars().filter(|c| !c.is_whitespace()) {
            let Some(actual) = stream.next() else {
                return Err(
                    "Punctuation output changed words; original punctuation retained.".into(),
                );
            };
            if !actual.eq_ignore_ascii_case(&expected) {
                return Err(
                    "Punctuation output changed words; original punctuation retained.".into(),
                );
            }
            casing.push(actual);
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
        result.push(WordPrediction {
            punctuation: mark,
            casing,
        });
    }
    if stream.next().is_some() {
        return Err("Punctuation output did not align; original punctuation retained.".into());
    }
    Ok(result)
}

fn punctuation_root() -> PathBuf {
    dirs_next::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("11th_echo/models/punctuation")
}
fn punctuation_directory() -> PathBuf {
    punctuation_root().join(MODEL_DIRECTORY)
}
fn punctuation_path() -> PathBuf {
    punctuation_directory().join(MODEL)
}
fn vocabulary_path() -> PathBuf {
    punctuation_directory().join(VOCABULARY)
}
pub fn punctuation_model_available() -> bool {
    punctuation_path()
        .metadata()
        .is_ok_and(|m| m.len() == MODEL_SIZE)
        && vocabulary_path()
            .metadata()
            .is_ok_and(|m| m.len() == VOCABULARY_SIZE)
}
fn load(model_path: &Path, vocabulary_path: &Path) -> Result<OnlinePunctuation, String> {
    if !model_path.is_file() || !vocabulary_path.is_file() {
        return Err(
            "Punctuation and capitalization model is missing. Enable post-processing in Settings to install it."
                .into(),
        );
    }
    if hash_file(model_path)? != MODEL_HASH || hash_file(vocabulary_path)? != VOCABULARY_HASH {
        return Err("Installed punctuation and capitalization model failed verification. Re-enable post-processing to repair it.".into());
    }
    OnlinePunctuation::create(&OnlinePunctuationConfig {
        model: OnlinePunctuationModelConfig {
            cnn_bilstm: Some(model_path.to_string_lossy().into_owned()),
            bpe_vocab: Some(vocabulary_path.to_string_lossy().into_owned()),
            num_threads: 1,
            provider: Some("cpu".into()),
            debug: false,
        },
    })
    .ok_or_else(|| {
        "Punctuation and capitalization model could not load. Re-enable post-processing to repair it."
            .into()
    })
}
fn punctuate(text: &str) -> Result<String, String> {
    let mut model = PUNCTUATOR
        .lock()
        .map_err(|_| "Punctuation and capitalization worker needs a restart.")?;
    if model.is_none() {
        *model = Some(load(&punctuation_path(), &vocabulary_path())?);
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
    let mut found_model = false;
    let mut found_vocabulary = false;
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?;
        let components = path
            .components()
            .filter(|c| !matches!(c, std::path::Component::CurDir))
            .collect::<Vec<_>>();
        if components.is_empty()
            || components.len() > 2
            || components
                .iter()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
            || components[0].as_os_str() != "sherpa-onnx-online-punct-en-2024-08-06"
        {
            return Err("Unsafe path in model archive.".into());
        }
        if components.len() == 1 && entry.header().entry_type().is_dir() {
            continue;
        }
        if components.len() != 2 {
            return Err("Unsafe path in model archive.".into());
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let (destination, expected_size, already_found) = match name {
            MODEL => (staged.join(MODEL), MODEL_SIZE, &mut found_model),
            VOCABULARY => (
                staged.join(VOCABULARY),
                VOCABULARY_SIZE,
                &mut found_vocabulary,
            ),
            _ => continue,
        };
        if *already_found || !entry.header().entry_type().is_file() || entry.size() != expected_size
        {
            return Err("Invalid punctuation model archive entry.".into());
        }
        let mut out = std::fs::File::create(destination).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        out.sync_all().map_err(|e| e.to_string())?;
        *already_found = true;
    }
    if !found_model || !found_vocabulary {
        return Err("Archive is missing punctuation and capitalization model files.".into());
    }
    if hash_file(&staged.join(MODEL))? != MODEL_HASH
        || hash_file(&staged.join(VOCABULARY))? != VOCABULARY_HASH
    {
        return Err("Extracted model files failed SHA-256 verification.".into());
    }
    Ok(())
}

pub async fn download_punctuation_model<F>(progress: F) -> Result<(), String>
where
    F: Fn(f32, String) + Send + Sync + 'static,
{
    download_to(punctuation_directory(), progress).await
}

async fn download_to<F>(destination: PathBuf, progress: F) -> Result<(), String>
where
    F: Fn(f32, String) + Send + Sync + 'static,
{
    let root = destination
        .parent()
        .ok_or("Cannot resolve punctuation model directory.")?
        .to_owned();
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|e| e.to_string())?;
    let archive = root.join("edge-punct-casing.tar.bz2.part");
    let staged = root.join("edge-punct-casing.installing");
    let backup = root.join("edge-punct-casing.previous");
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
    tokio::task::spawn_blocking(move || {
        progress(0.85, "Verifying and unpacking model…".into());
        if staged.exists() {
            std::fs::remove_dir_all(&staged).map_err(|e| e.to_string())?;
        }
        std::fs::create_dir(&staged).map_err(|e| e.to_string())?;
        extract_verified(&archive, &staged)?;
        progress(
            0.93,
            "Loading model and testing punctuation and capitalization…".into(),
        );
        let model = load(&staged.join(MODEL), &staged.join(VOCABULARY))?;
        let probe = model
            .add_punctuation("how are you i am fine thank you")
            .ok_or("Model inference test failed.")?;
        let predictions = word_predictions(
            &[
                "how".into(),
                "are".into(),
                "you".into(),
                "i".into(),
                "am".into(),
                "fine".into(),
                "thank".into(),
                "you".into(),
            ],
            &probe,
        )?;
        if !predictions[0].casing.starts_with('H')
            || !predictions[2].punctuation.ends_with('?')
            || !predictions[3].casing.starts_with('I')
            || !predictions[5].punctuation.ends_with('.')
        {
            return Err(
                "Model inference test did not restore expected casing and punctuation.".into(),
            );
        }
        let mut current = PUNCTUATOR
            .lock()
            .map_err(|_| "Punctuation and capitalization worker needs a restart.")?;
        if backup.exists() {
            std::fs::remove_dir_all(&backup).map_err(|e| e.to_string())?;
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
            let _ = std::fs::remove_dir_all(&backup);
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
    fn learned_casing_applies_inside_a_sentence() {
        let result = process_with(
            "i met alice yesterday",
            &PostProcessingSettings::default(),
            |input| {
                assert_eq!(input, "i met alice yesterday");
                Ok("I met Alice yesterday.".into())
            },
        );
        assert_eq!(result.text, "I met Alice yesterday.");
        assert!(result.warning.is_none());
    }
    #[test]
    fn checksum_is_full_sha256() {
        assert_eq!(ARCHIVE_HASH.len(), 64);
    }
    #[test]
    fn model_cannot_change_words() {
        assert!(word_predictions(&["hello".into()], "goodbye.").is_err());
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
        let staged = dir.join("staged");
        std::fs::create_dir(&staged).unwrap();
        let model = staged.join(MODEL);
        std::fs::write(&archive, b"truncated").unwrap();
        std::fs::write(&model, b"existing model").unwrap();
        assert!(extract_verified(&archive, &staged).is_err());
        assert_eq!(std::fs::read(&model).unwrap(), b"existing model");
        std::fs::remove_file(archive).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    #[ignore = "requires locally downloaded verified archive; runs real CPU inference"]
    fn native_model_smoke() {
        let archive =
            std::env::var_os("ECHO_PUNCTUATION_TEST_ARCHIVE").expect("set test archive path");
        let dir =
            std::env::temp_dir().join(format!("echo-punctuation-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        extract_verified(Path::new(&archive), &dir).unwrap();
        let model_path = dir.join(MODEL);
        let vocabulary = dir.join(VOCABULARY);
        eprintln!("Verified model SHA-256 {}", hash_file(&model_path).unwrap());
        let start = Instant::now();
        let model = load(&model_path, &vocabulary).unwrap();
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
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "requires cached verified archive; tests installation in a temporary directory without downloading"]
    fn cached_model_installation_smoke() {
        let source =
            std::env::var_os("ECHO_PUNCTUATION_TEST_ARCHIVE").expect("set test archive path");
        let dir = std::env::temp_dir().join(format!("echo-install-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(source, dir.join("edge-punct-casing.tar.bz2.part")).unwrap();
        let destination = dir.join(MODEL_DIRECTORY);
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join(MODEL), b"previous broken model").unwrap();
        let updates = std::sync::Arc::new(Mutex::new(Vec::new()));
        let captured = updates.clone();
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(download_to(destination.clone(), move |progress, status| {
                eprintln!("{:.0}% {status}", progress * 100.0);
                captured.lock().unwrap().push(progress);
            }))
            .unwrap();
        assert_eq!(hash_file(&destination.join(MODEL)).unwrap(), MODEL_HASH);
        assert_eq!(
            hash_file(&destination.join(VOCABULARY)).unwrap(),
            VOCABULARY_HASH
        );
        assert!(!dir.join("edge-punct-casing.tar.bz2.part").exists());
        assert!(!dir.join("edge-punct-casing.previous").exists());
        assert_eq!(updates.lock().unwrap().last(), Some(&1.0));
        *PUNCTUATOR.lock().unwrap() = None;
        std::fs::remove_dir_all(dir).unwrap();
    }
}
