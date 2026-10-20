//! Calendar notifications are local presentation state, independent of agent execution.
mod cache;
mod ipc;
pub mod model;
mod outlook;
mod store;
#[cfg(test)]
mod tests;
pub mod ui;
use model::*;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, LazyLock, Mutex,
    },
};
use store::{Account, Store};

static INSTANCE: LazyLock<Mutex<Option<Arc<Service>>>> = LazyLock::new(|| Mutex::new(None));
pub struct Service {
    store: Mutex<Store>,
    pub revision: AtomicU64,
    stop: AtomicBool,
    pub last_spoken: Mutex<String>,
    speech_error: Mutex<String>,
    pending: Mutex<HashMap<String, Notice>>,
    settings: Arc<Mutex<crate::settings::AppSettings>>,
    busy: AtomicBool,
    display: Mutex<Option<(Notice, std::time::Instant)>>,
    playing: Mutex<Option<Notice>>,
    valid: Mutex<HashMap<String, Notice>>,
    sync_requested: AtomicBool,
}
pub fn directory() -> std::path::PathBuf {
    std::env::var_os("ECHO_REMINDERS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate::settings::settings_path().with_file_name("reminders"))
}
pub fn instance() -> Option<Arc<Service>> {
    INSTANCE.lock().unwrap().clone()
}
pub fn cli() -> Result<(), String> {
    ipc::cli()
}
pub fn notification() -> Option<String> {
    instance()?.visible_notice().map(|n| n.text)
}
pub fn speech_started(_text: &str, generation: u64) {
    if generation != crate::assistant_voice::playback_generation() {
        return;
    }
    if let Some(service) = instance() {
        let notice = service.playing.lock().unwrap().clone();
        if let Some(notice) = notice.filter(|n| service.eligible(n)) {
            *service.last_spoken.lock().unwrap() = notice.occurrence.key.clone();
            service.publish(&notice, true);
        }
    }
}
impl Service {
    fn create(
        path: &std::path::Path,
        settings: Arc<Mutex<crate::settings::AppSettings>>,
    ) -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            store: Mutex::new(Store::open(path)?),
            revision: AtomicU64::new(1),
            stop: AtomicBool::new(false),
            last_spoken: Mutex::new(String::new()),
            speech_error: Mutex::new(String::new()),
            pending: Mutex::new(HashMap::new()),
            settings,
            busy: AtomicBool::new(false),
            display: Mutex::new(None),
            playing: Mutex::new(None),
            valid: Mutex::new(HashMap::new()),
            sync_requested: AtomicBool::new(false),
        }))
    }
    pub fn start(settings: Arc<Mutex<crate::settings::AppSettings>>) -> Result<Arc<Self>, String> {
        let service = Self::create(&directory().join("reminders.sqlite"), settings)?;
        *INSTANCE.lock().unwrap() = Some(service.clone());
        let tick = service.clone();
        std::thread::spawn(move || {
            while !tick.stop.load(Ordering::SeqCst) {
                if let Err(error) = tick.tick(chrono::Utc::now().timestamp()) {
                    *tick.speech_error.lock().unwrap() = error;
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        });
        let audio = service.clone();
        std::thread::spawn(move || audio.speech());
        cache::start(service.clone());
        let sync = service.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            while !sync.stop.load(Ordering::SeqCst) {
                if !sync.busy.swap(true, Ordering::SeqCst) {
                    sync.changed();
                    let _ = runtime.block_on(sync.sync());
                    sync.busy.store(false, Ordering::SeqCst);
                    sync.changed();
                }
                let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
                let wall_until = chrono::Utc::now().timestamp() + 60;
                while std::time::Instant::now() < until
                    && chrono::Utc::now().timestamp() < wall_until
                {
                    if sync.stop.load(Ordering::SeqCst)
                        || sync.sync_requested.swap(false, Ordering::SeqCst)
                    {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        });
        ipc::start(service.clone());
        Ok(service)
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
    fn changed(&self) {
        self.revision.fetch_add(1, Ordering::SeqCst);
    }
    fn publish(&self, notice: &Notice, force: bool) {
        let valid = self.valid.lock().unwrap();
        if !valid.get(&notice.occurrence.key).is_some_and(|n| {
            n.ticket == notice.ticket && n.occurrence.title == notice.occurrence.title
        }) {
            return;
        }
        let mut display = self.display.lock().unwrap();
        if force
            || display.as_ref().is_none_or(|(n, _)| {
                n.ticket != notice.ticket || n.occurrence.key != notice.occurrence.key
            })
        {
            *display = Some((
                notice.clone(),
                std::time::Instant::now() + std::time::Duration::from_secs(15),
            ));
            self.changed();
        }
    }
    pub fn visible_notice(&self) -> Option<Notice> {
        let (notice, until) = self.display.lock().unwrap().clone()?;
        if std::time::Instant::now() > until || self.stop.load(Ordering::SeqCst) {
            return None;
        }
        if self
            .valid
            .lock()
            .unwrap()
            .get(&notice.occurrence.key)
            .is_none_or(|n| n.ticket != notice.ticket)
        {
            return None;
        }
        Some(notice)
    }
    pub fn snapshot(&self, offset: usize) -> Result<Value, String> {
        let store = self.store.lock().unwrap();
        let calendars = store.calendars()?;
        let events = store.events(offset, 41)?;
        let more = events.len() > 40;
        let events=events.into_iter().take(40).map(|(event,progress)| {
            let resolved=store.resolve(&event)?;let enabled=resolved.is_some();let event=resolved.unwrap_or(event);let rule=store.rule(&event)?;
            Ok(json!({"enabled":enabled&&rule.enabled,"event":event,"progress":progress,"rule":rule}))
        }).collect::<Result<Vec<_>,String>>()?;
        let accounts = store
            .accounts()?
            .into_iter()
            .map(|a| json!({"id":a.id,"name":a.name,"status":a.status,"last_sync":a.last_sync}))
            .collect::<Vec<_>>();
        Ok(
            json!({"accounts":accounts,"calendars":calendars,"events":events,"offset":offset,"more":more,"error":self.speech_error.lock().unwrap().clone(),"busy":self.busy.load(Ordering::SeqCst),"client_id":store.meta("client_id")?,"global":store.rule_for_scope("global")?}),
        )
    }
    fn tick(&self, now: i64) -> Result<(), String> {
        let store = self.store.lock().unwrap();
        let mut offset = 0;
        let mut notices = Vec::new();
        let mut valid = HashMap::new();
        loop {
            let events = store.candidates(now, offset, 128)?;
            if events.is_empty() {
                break;
            }
            offset += events.len();
            for (event, mut progress) in events {
                if progress.dismissed {
                    continue;
                }
                let Some(event) = store.resolve(&event)? else {
                    continue;
                };
                let rule = store.rule(&event)?;
                let mut fired = false;
                if let Some(notice) = due(&event, &progress, &rule, now) {
                    fired = true;
                    progress.activated = true;
                    progress.snooze_until = 0;
                    progress.last_ticket = notice.ticket.clone();
                    progress.last_text = notice.text.clone();
                    store.progress(&event.key, &progress)?;
                    notices.push(notice);
                }
                let trigger = event.trigger(&rule);
                let first_offset = if event.all_day.is_some() {
                    0
                } else {
                    rule.offsets.iter().copied().max().unwrap_or(0)
                };
                let phase_ready = now >= trigger - first_offset * 60
                    && progress.last_ticket.starts_with(&format!("{trigger}/"));
                if phase_ready
                    && progress.activated
                    && !progress.dismissed
                    && progress.snooze_until <= now
                    && rule.enabled
                    && !event.cancelled
                {
                    valid.insert(
                        event.key.clone(),
                        Notice {
                            occurrence: event.clone(),
                            ticket: progress.last_ticket.clone(),
                            text: progress.last_text.clone(),
                            rule: rule.clone(),
                        },
                    );
                }
                if !fired
                    && phase_ready
                    && progress.activated
                    && !progress.dismissed
                    && progress.snooze_until <= now
                    && progress.last_ticket != progress.spoken_ticket
                    && rule.enabled
                    && !event.cancelled
                {
                    notices.push(Notice {
                        occurrence: event,
                        ticket: progress.last_ticket.clone(),
                        text: progress.last_text,
                        rule,
                    });
                }
            }
        }
        *self.valid.lock().unwrap() = valid;
        drop(store);
        {
            let valid = self
                .valid
                .lock()
                .unwrap()
                .keys()
                .cloned()
                .collect::<std::collections::HashSet<_>>();
            self.pending
                .lock()
                .unwrap()
                .retain(|key, _| valid.contains(key));
        }
        if !notices.is_empty() {
            if let Some(n) = notices.iter().min_by_key(|n| n.occurrence.start) {
                self.publish(n, false);
            }
            let mut pending = self.pending.lock().unwrap();
            let mut changed = false;
            for n in notices {
                changed |= pending
                    .get(&n.occurrence.key)
                    .is_none_or(|p| p.ticket != n.ticket || p.text != n.text || p.rule != n.rule);
                pending.insert(n.occurrence.key.clone(), n);
            }
            drop(pending);
            if changed {
                self.changed();
            }
        }
        Ok(())
    }
    pub fn eligible(&self, notice: &Notice) -> bool {
        if self.stop.load(Ordering::SeqCst)
            || !notice.rule.spoken
            || notice.rule.quiet_now(chrono::Utc::now())
        {
            return false;
        }
        self.valid
            .lock()
            .unwrap()
            .get(&notice.occurrence.key)
            .is_some_and(|n| {
                n.ticket == notice.ticket
                    && n.rule == notice.rule
                    && n.occurrence.start == notice.occurrence.start
                    && n.occurrence.title == notice.occurrence.title
            })
    }

    fn speech(self: &Arc<Self>) {
        while !self.stop.load(Ordering::SeqCst) {
            if crate::assistant_presentation::runtime_snapshot().listening {
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
            let next = self
                .pending
                .lock()
                .unwrap()
                .values()
                .filter(|n| self.eligible(n))
                .min_by_key(|n| n.occurrence.start)
                .cloned();
            if let Some(notice) = next {
                let generation = crate::assistant_voice::playback_generation();
                let voice =
                    crate::voices::resolve(&notice.rule.voice, &self.settings.lock().unwrap());
                let service = self.clone();
                let check = notice.clone();
                *self.playing.lock().unwrap() = Some(notice.clone());
                let cached = cache::path(&notice.text, &voice).filter(|p| p.is_file());
                let result = if let Some(path) = cached {
                    crate::assistant_voice::play_cached_reminder(
                        &path,
                        &notice.text,
                        generation,
                        || {
                            !self.eligible(&notice)
                                || crate::assistant_presentation::runtime_snapshot().listening
                        },
                    )
                } else {
                    crate::assistant_voice::play_reminder(
                        &notice.text,
                        &voice,
                        generation,
                        Arc::new(move || {
                            !service.eligible(&check)
                                || crate::assistant_presentation::runtime_snapshot().listening
                        }),
                    )
                    .map(|_| true)
                };
                *self.playing.lock().unwrap() = None;
                match result {
                    Ok(true)
                        if generation == crate::assistant_voice::playback_generation()
                            && self.eligible(&notice) =>
                    {
                        let store = self.store.lock().unwrap();
                        if let Ok((_, mut p)) = store.event(&notice.occurrence.key) {
                            p.spoken_ticket = notice.ticket.clone();
                            let _ = store.progress(&notice.occurrence.key, &p);
                        }
                        self.pending.lock().unwrap().remove(&notice.occurrence.key);
                    }
                    Err(error) => {
                        *self.speech_error.lock().unwrap() = error;
                        self.changed();
                        std::thread::sleep(std::time::Duration::from_secs(10));
                    }
                    _ => {}
                }
            } else {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
    }
    pub fn command(&self, args: Value) -> Result<Value, String> {
        let op = args["action"].as_str().unwrap_or("list");
        if op == "get_rule" {
            return Ok(json!({"rule":self.rule(args["scope"].as_str().unwrap_or("global"))?}));
        }
        if op == "list" {
            return self.snapshot(args["offset"].as_u64().unwrap_or_default().min(100000) as usize);
        }
        let mut target = if matches!(op, "dismiss" | "snooze" | "enable") {
            args["key"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .or_else(|| self.visible_notice().map(|n| n.occurrence.key))
                .unwrap_or_else(|| self.last_spoken.lock().unwrap().clone())
        } else {
            String::new()
        };
        let store = self.store.lock().unwrap();
        if matches!(op, "dismiss" | "snooze")
            && args["key"].as_str().is_none_or(str::is_empty)
            && (target.is_empty() || store.event(&target).map_or(true, |(_, p)| p.dismissed))
        {
            let active = self.valid.lock().unwrap();
            target = if active.len() == 1 {
                active.keys().next().unwrap().clone()
            } else {
                return Err(
                    "Choose which event to dismiss or snooze in Calendar & Reminders.".into(),
                );
            };
        }
        match op {
            "client_id" => {
                let id = args["value"].as_str().unwrap_or_default().trim();
                outlook::validate_client(id)?;
                store.set_meta("client_id", id)?;
            }
            "calendar" => {
                let key = args["key"].as_str().unwrap_or_default();
                let mut c = store
                    .calendars()?
                    .into_iter()
                    .find(|c| c.key == key)
                    .ok_or("Calendar not found.")?;
                c.enabled = args["enabled"]
                    .as_bool()
                    .ok_or("Specify enabled or disabled.")?;
                store.save_calendar(&c)?;
            }
            "remove_account" => {
                store.remove_account(args["key"].as_str().unwrap_or_default())?;
            }
            "rule" => {
                let scope = args["scope"].as_str().unwrap_or("global");
                let mut merged = serde_json::to_value(store.rule_for_scope(scope)?)
                    .map_err(|_| "Invalid reminder settings.")?;
                let fields = args["rule"].as_object().ok_or("Rule must be an object.")?;
                merged
                    .as_object_mut()
                    .unwrap()
                    .extend(fields.iter().map(|(k, v)| (k.clone(), v.clone())));
                let rule: Rule =
                    serde_json::from_value(merged).map_err(|_| "Invalid reminder settings.")?;
                store.set_rule(args["scope"].as_str().unwrap_or("global"), rule)?;
            }
            "inherit" => {
                store.remove_rule(args["scope"].as_str().ok_or("Choose a rule scope.")?)?
            }
            "dismiss" | "snooze" | "enable" => {
                let key = target;
                if key.is_empty() {
                    return Err("Choose the event to dismiss or snooze.".into());
                }
                let (e, mut p) = store.event(&key)?;
                match op {
                    "dismiss" => p.dismissed = true,
                    "enable" => {
                        p.dismissed = false;
                        p.last_ticket.clear();
                    }
                    _ => {
                        let minutes = args["minutes"]
                            .as_i64()
                            .unwrap_or(store.rule(&e)?.snooze_minutes);
                        if !(1..=10080).contains(&minutes) {
                            return Err("Snooze must be 1–10080 minutes.".into());
                        }
                        p.snooze_until = chrono::Utc::now().timestamp() + minutes * 60;
                    }
                }
                store.progress(&key, &p)?;
            }
            _ => return Err("Unknown reminder action.".into()),
        }
        self.valid.lock().unwrap().clear();
        drop(store);
        self.display.lock().unwrap().take();
        self.pending.lock().unwrap().clear();
        self.speech_error.lock().unwrap().clear();
        self.changed();
        Ok(json!({"status":"saved"}))
    }
    pub fn rule(&self, scope: &str) -> Result<Rule, String> {
        self.store.lock().unwrap().rule_for_scope(scope)
    }
    pub fn connect(self: &Arc<Self>, client_id: String) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        *self.speech_error.lock().unwrap() = String::new();
        self.changed();
        let service = self.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let prepared = {
                service
                    .store
                    .lock()
                    .unwrap()
                    .set_meta("client_id", &client_id)
            };
            let result = prepared.and_then(|_| runtime.block_on(outlook::login(&client_id)));
            match result {
                Ok((account, calendars)) => {
                    let store = service.store.lock().unwrap();
                    let result = (|| {
                        store.save_account(&account)?;
                        store.set_meta("client_id", &client_id)?;
                        for mut c in calendars {
                            if let Some(previous) =
                                store.calendars()?.into_iter().find(|p| p.key == c.key)
                            {
                                c.enabled = previous.enabled;
                            }
                            store.save_calendar(&c)?;
                        }
                        Ok::<_, String>(())
                    })();
                    if let Err(e) = result {
                        *service.speech_error.lock().unwrap() = e;
                    }
                }
                Err(e) => *service.speech_error.lock().unwrap() = e,
            }
            service.busy.store(false, Ordering::SeqCst);
            service.changed();
            service.refresh();
        });
    }
    pub fn refresh(self: &Arc<Self>) {
        if self.busy.swap(true, Ordering::SeqCst) {
            self.sync_requested.store(true, Ordering::SeqCst);
            return;
        }
        let service = self.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _ = rt.block_on(service.sync());
            service.busy.store(false, Ordering::SeqCst);
            service.changed();
        });
    }
    async fn sync(&self) -> Result<(), String> {
        let accounts = self.store.lock().unwrap().accounts()?;
        for account in accounts {
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            let result = self.sync_account(&account).await;
            if let Err(e) = result {
                let store = self.store.lock().unwrap();
                if let Some(mut current) =
                    store.accounts()?.into_iter().find(|a| a.id == account.id)
                {
                    current.status = e;
                    let _ = store.save_account(&current);
                }
            }
        }
        self.changed();
        Ok(())
    }
    async fn sync_account(&self, account: &Account) -> Result<(), String> {
        let (token, mut account) = outlook::refresh(account).await?;
        {
            let store = self.store.lock().unwrap();
            if !store.accounts()?.iter().any(|a| a.id == account.id) {
                return Ok(());
            }
            store.save_account(&account)?;
        }
        let fresh = outlook::calendars(&account, &token).await?;
        {
            let store = self.store.lock().unwrap();
            let previous = store.calendars()?;
            if !store.accounts()?.iter().any(|a| a.id == account.id) {
                return Ok(());
            }
            for mut c in fresh.clone() {
                if let Some(old) = previous.iter().find(|old| old.key == c.key) {
                    c.enabled = old.enabled;
                }
                store.save_calendar(&c)?;
            }
            for mut c in previous
                .into_iter()
                .filter(|c| c.account == account.id && !fresh.iter().any(|f| f.key == c.key))
            {
                c.enabled = false;
                store.save_calendar(&c)?;
            }
        }
        let calendars = self
            .store
            .lock()
            .unwrap()
            .calendars()?
            .into_iter()
            .filter(|c| c.account == account.id && c.enabled)
            .collect::<Vec<_>>();
        for calendar in calendars {
            let now = chrono::Utc::now().timestamp();
            let mut events = outlook::events(&account, &calendar, &token, now).await?;
            let previous = self.store.lock().unwrap().sources(&calendar.key)?;
            let ids = events
                .iter()
                .map(|e| e.provider_id.clone())
                .collect::<std::collections::HashSet<_>>();
            for mut event in previous
                .into_iter()
                .filter(|e| !e.cancelled && !ids.contains(&e.provider_id))
            {
                let relevant = event.start >= now - 86400
                    || self
                        .store
                        .lock()
                        .unwrap()
                        .event(&event.key)
                        .is_ok_and(|(_, p)| p.activated && !p.dismissed);
                if !relevant {
                    continue;
                }
                if let Some(updated) =
                    outlook::missing(&account, &calendar, &token, &event.provider_id).await?
                {
                    events.push(updated);
                } else {
                    event.cancelled = true;
                    events.push(event);
                }
            }
            let store = self.store.lock().unwrap();
            // An account/calendar disabled during HTTP work must stay disabled.
            if !store.accounts()?.iter().any(|a| a.id == account.id)
                || !store
                    .calendars()?
                    .iter()
                    .any(|c| c.key == calendar.key && c.enabled)
            {
                continue;
            }
            let transaction = store
                .db
                .unchecked_transaction()
                .map_err(|_| "Could not commit calendar synchronization.")?;
            for event in &events {
                store.upsert(event)?;
            }
            transaction
                .commit()
                .map_err(|_| "Could not commit calendar synchronization.")?;
            let mut valid = self.valid.lock().unwrap();
            valid.retain(|key, n| {
                store
                    .event(key)
                    .ok()
                    .and_then(|(e, _)| store.resolve(&e).ok().flatten())
                    .is_some_and(|e| e.start == n.occurrence.start && e.title == n.occurrence.title)
            });
            let mut display = self.display.lock().unwrap();
            if display
                .as_ref()
                .is_some_and(|(n, _)| !valid.contains_key(&n.occurrence.key))
            {
                display.take();
            }
        }
        account.status = "Connected".into();
        account.last_sync = chrono::Utc::now().timestamp();
        let store = self.store.lock().unwrap();
        if store.accounts()?.iter().any(|a| a.id == account.id) {
            store.save_account(&account)?;
        }
        Ok(())
    }
}
pub fn local_command(text: &str) -> bool {
    let normalized = text.trim().trim_end_matches(['.', '!', '?']).to_lowercase();
    let action = match normalized.as_str() {
        "stop reminding me" | "dismiss this reminder" | "dismiss the reminder" => "dismiss",
        "snooze this reminder" | "snooze the reminder" => "snooze",
        _ => return false,
    };
    let Some(service) = instance() else {
        return false;
    };
    match service.command(json!({"action":action})) {
        Ok(_) => {
            crate::assistant_presentation::update(|s| s.state = "Reminder updated".into());
            true
        }
        Err(_) => false,
    }
}
