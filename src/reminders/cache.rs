use super::*;
use std::path::{Path, PathBuf};
pub fn path(text: &str, voice: &Path) -> Option<PathBuf> {
    use sha2::{Digest, Sha256};
    let identity = crate::assistant_voice::phrase_fingerprint(voice).ok()?;
    Some(directory().join("audio").join(format!(
        "{:x}.wav",
        Sha256::digest(format!("{identity}\n{text}").as_bytes())
    )))
}
pub fn start(service: Arc<Service>) {
    std::thread::spawn(move || {
        while !service.stop.load(Ordering::SeqCst) {
            if !crate::assistant_presentation::runtime_snapshot().listening
                && crate::assistant_voice::model_readiness()
                    == crate::assistant_voice::ModelReadiness::Ready
            {
                let now = chrono::Utc::now().timestamp();
                let work = (|| -> Result<Vec<(String, Rule)>, String> {
                    let store = service.store.lock().unwrap();
                    let mut work = Vec::new();
                    for (event, p) in store.candidates(now, 0, 256)? {
                        if p.dismissed {
                            continue;
                        }
                        let Some(event) = store.resolve(&event)? else {
                            continue;
                        };
                        let rule = store.rule(&event)?;
                        if !rule.enabled
                            || !rule.spoken
                            || event.start < now
                            || event.start > now + 7200
                        {
                            continue;
                        }
                        for offset in &rule.offsets {
                            if event.trigger(&rule) - offset * 60 < now || work.len() >= 64 {
                                continue;
                            }
                            if let Some(n) = due(
                                &event,
                                &Progress::default(),
                                &rule,
                                event.trigger(&rule) - offset * 60,
                            ) {
                                work.push((n.text, rule.clone()));
                            }
                        }
                    }
                    Ok(work)
                })()
                .unwrap_or_default();
                for (text, rule) in work {
                    let voice =
                        crate::voices::resolve(&rule.voice, &service.settings.lock().unwrap());
                    let Some(path) = path(&text, &voice) else {
                        break;
                    };
                    if path.exists() {
                        continue;
                    }
                    if std::fs::create_dir_all(path.parent().unwrap()).is_err() {
                        break;
                    }
                    let _ = crate::assistant_voice::warm_reminder(&text, &voice, &path, || {
                        service.stop.load(Ordering::SeqCst)
                            || crate::assistant_presentation::runtime_snapshot().listening
                    });
                    prune(path.parent().unwrap());
                }
            }
            for _ in 0..30 {
                if service.stop.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    });
}
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            if name.len() != 68
                || !name.ends_with(".wav")
                || !name[..64].bytes().all(|b| b.is_ascii_hexdigit())
            {
                return None;
            }
            let m = e.metadata().ok()?;
            if !m.is_file() {
                return None;
            }
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|f| f.0);
    let mut total = files.iter().map(|f| f.1).sum::<u64>();
    let mut count = files.len();
    for (_, len, path) in files {
        if count <= 64 && total <= 32 * 1024 * 1024 {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            count -= 1;
            total -= len;
        }
    }
}
