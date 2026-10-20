use super::model::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub client_id: String,
    pub refresh: String,
    pub status: String,
    pub last_sync: i64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Calendar {
    pub key: String,
    pub id: String,
    pub account: String,
    pub name: String,
    pub enabled: bool,
}
pub struct Store {
    pub db: Connection,
}
pub type Result<T> = std::result::Result<T, String>;
fn error(_: impl std::fmt::Display) -> String {
    "Could not access the local calendar/reminder database.".into()
}
impl Store {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(error)?;
        }
        let db = Connection::open(path).map_err(error)?;
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(error)?;
        if version > 1 {
            return Err("This reminder database requires a newer Echo version.".into());
        }
        db.busy_timeout(std::time::Duration::from_secs(3))
            .map_err(error)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS accounts(id TEXT PRIMARY KEY,json TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS calendars(key TEXT PRIMARY KEY,account TEXT NOT NULL,json TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS sources(id TEXT PRIMARY KEY,occurrence TEXT NOT NULL,calendar TEXT NOT NULL,json TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS source_occurrence ON sources(occurrence);
            CREATE TABLE IF NOT EXISTS events(key TEXT PRIMARY KEY,start INTEGER NOT NULL,json TEXT NOT NULL,progress TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS event_start ON events(start);
            CREATE INDEX IF NOT EXISTS event_active ON events(start) WHERE json_extract(progress,'$.activated')=1 AND json_extract(progress,'$.dismissed')=0;
            CREATE TABLE IF NOT EXISTS rules(scope TEXT PRIMARY KEY,json TEXT NOT NULL);
            PRAGMA user_version=1;").map_err(error)?;
        Ok(Self { db })
    }
    pub fn meta(&self, key: &str) -> Result<String> {
        Ok(self
            .db
            .query_row("SELECT value FROM meta WHERE key=?", [key], |r| r.get(0))
            .optional()
            .map_err(error)?
            .unwrap_or_default())
    }
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.db
            .execute(
                "INSERT INTO meta VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            )
            .map_err(error)?;
        Ok(())
    }
    pub fn accounts(&self) -> Result<Vec<Account>> {
        self.json_rows("SELECT json FROM accounts", [])
    }
    pub fn calendars(&self) -> Result<Vec<Calendar>> {
        self.json_rows("SELECT json FROM calendars ORDER BY key", [])
    }
    fn json_rows<T: serde::de::DeserializeOwned, P: rusqlite::Params>(
        &self,
        sql: &str,
        p: P,
    ) -> Result<Vec<T>> {
        let mut stmt = self.db.prepare(sql).map_err(error)?;
        let rows = stmt
            .query_map(p, |r| r.get::<_, String>(0))
            .map_err(error)?;
        rows.map(|s| serde_json::from_str(&s.map_err(error)?).map_err(error))
            .collect()
    }
    pub fn save_account(&self, a: &Account) -> Result<()> {
        self.db
            .execute(
                "INSERT INTO accounts VALUES(?,?) ON CONFLICT(id) DO UPDATE SET json=excluded.json",
                params![a.id, serde_json::to_string(a).map_err(error)?],
            )
            .map_err(error)?;
        Ok(())
    }
    pub fn save_calendar(&self, c: &Calendar) -> Result<()> {
        self.db.execute("INSERT INTO calendars VALUES(?,?,?) ON CONFLICT(key) DO UPDATE SET json=excluded.json",params![c.key,c.account,serde_json::to_string(c).map_err(error)?]).map_err(error)?;
        Ok(())
    }
    pub fn rule(&self, e: &Occurrence) -> Result<Rule> {
        for scope in [
            format!("occurrence:{}", e.key),
            format!("series:{}:{}", e.account, e.series),
            format!("calendar:{}", e.calendar),
            "global".into(),
        ] {
            if let Some(json) = self
                .db
                .query_row("SELECT json FROM rules WHERE scope=?", [scope], |r| {
                    r.get::<_, String>(0)
                })
                .optional()
                .map_err(error)?
            {
                let mut rule: Rule = serde_json::from_str(&json).map_err(error)?;
                rule.validate()?;
                return Ok(rule);
            }
        }
        Ok(Rule::default())
    }
    pub fn rule_for_scope(&self, scope: &str) -> Result<Rule> {
        if let Some(s) = self
            .db
            .query_row("SELECT json FROM rules WHERE scope=?", [scope], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .map_err(error)?
        {
            let mut rule: Rule = serde_json::from_str(&s).map_err(error)?;
            rule.validate()?;
            return Ok(rule);
        }
        if let Some(key) = scope.strip_prefix("occurrence:") {
            let (e, _) = self.event(key)?;
            return self.rule(&e);
        }
        if let Some(series) = scope.strip_prefix("series:") {
            if let Some((account, series)) = series.split_once(':') {
                let events:Vec<Occurrence>=self.json_rows("SELECT json FROM events WHERE json_extract(json,'$.account')=? AND json_extract(json,'$.series')=? LIMIT 1",params![account,series])?;
                if let Some(e) = events.first() {
                    return self.rule_for_scope(&format!("calendar:{}", e.calendar));
                }
            }
        }
        if scope != "global" {
            return self.rule_for_scope("global");
        }
        Ok(Rule::default())
    }
    pub fn set_rule(&self, scope: &str, mut rule: Rule) -> Result<()> {
        if scope.len() > 1024
            || !(scope == "global"
                || scope.starts_with("calendar:")
                || scope.starts_with("series:")
                || scope.starts_with("occurrence:"))
        {
            return Err("Invalid reminder rule scope.".into());
        }
        rule.validate()?;
        self.db
            .execute(
                "INSERT INTO rules VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET json=excluded.json",
                params![scope, serde_json::to_string(&rule).map_err(error)?],
            )
            .map_err(error)?;
        Ok(())
    }
    pub fn remove_rule(&self, scope: &str) -> Result<()> {
        self.db
            .execute("DELETE FROM rules WHERE scope=?", [scope])
            .map_err(error)?;
        Ok(())
    }
    pub fn sources(&self, calendar: &str) -> Result<Vec<Occurrence>> {
        self.json_rows("SELECT json FROM sources WHERE calendar=?", [calendar])
    }
    pub fn upsert(&self, e: &Occurrence) -> Result<()> {
        let source = format!("{}:{}", e.account, e.provider_id);
        let key = self
            .db
            .query_row(
                "SELECT occurrence FROM sources WHERE id=?",
                [&source],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(error)?
            .unwrap_or_else(|| e.key.clone());
        let mut e = e.clone();
        e.key = key;
        let previous = self.event(&e.key).ok();
        self.db.execute("INSERT INTO sources VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET json=excluded.json,calendar=excluded.calendar",params![source,e.key,e.calendar,serde_json::to_string(&e).map_err(error)?]).map_err(error)?;
        self.db.execute("INSERT INTO events VALUES(?,?,?,?) ON CONFLICT(key) DO UPDATE SET start=excluded.start,json=excluded.json",params![e.key,e.start,serde_json::to_string(&e).map_err(error)?,serde_json::to_string(&Progress::default()).map_err(error)?]).map_err(error)?;
        if let Some((old, mut state)) = previous {
            if !state.dismissed && (old.start != e.start || old.all_day != e.all_day) {
                state = Progress::default();
            } else if old.title != e.title {
                state.last_text = state.last_text.replace(&old.title, &e.title);
            }
            self.progress(&e.key, &state)?;
        }
        Ok(())
    }
    pub fn events(&self, offset: usize, limit: usize) -> Result<Vec<(Occurrence, Progress)>> {
        let mut stmt = self
            .db
            .prepare("SELECT json,progress FROM events ORDER BY CASE WHEN json_extract(progress,'$.activated')=1 AND json_extract(progress,'$.dismissed')=0 THEN 0 WHEN start>=unixepoch() THEN 1 ELSE 2 END,start,key LIMIT ? OFFSET ?")
            .map_err(error)?;
        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(error)?;
        rows.map(|v| {
            let (a, b) = v.map_err(error)?;
            Ok((
                serde_json::from_str(&a).map_err(error)?,
                serde_json::from_str(&b).map_err(error)?,
            ))
        })
        .collect()
    }
    pub fn candidates(
        &self,
        now: i64,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<(Occurrence, Progress)>> {
        let max_minutes:i64=self.db.query_row("SELECT COALESCE(MAX(CAST(value AS INTEGER)),60) FROM rules,json_each(rules.json,'$.offsets')",[],|r|r.get(0)).map_err(error)?;
        let horizon = (max_minutes.clamp(60, 43200) * 60).max(86400);
        let mut stmt=self.db.prepare("SELECT json,progress FROM events WHERE (start>=? AND start<=?) OR (json_extract(progress,'$.activated')=1 AND json_extract(progress,'$.dismissed')=0) ORDER BY start,key LIMIT ? OFFSET ?").map_err(error)?;
        let rows = stmt
            .query_map(
                params![now - 86400, now + horizon, limit as i64, offset as i64],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(error)?;
        rows.map(|v| {
            let (a, b) = v.map_err(error)?;
            Ok((
                serde_json::from_str(&a).map_err(error)?,
                serde_json::from_str(&b).map_err(error)?,
            ))
        })
        .collect()
    }
    pub fn event(&self, key: &str) -> Result<(Occurrence, Progress)> {
        let (a, b): (String, String) = self
            .db
            .query_row("SELECT json,progress FROM events WHERE key=?", [key], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(|_| "Reminder not found.".to_string())?;
        Ok((
            serde_json::from_str(&a).map_err(error)?,
            serde_json::from_str(&b).map_err(error)?,
        ))
    }
    pub fn progress(&self, key: &str, p: &Progress) -> Result<()> {
        self.db
            .execute(
                "UPDATE events SET progress=? WHERE key=?",
                params![serde_json::to_string(p).map_err(error)?, key],
            )
            .map_err(error)?;
        Ok(())
    }
    pub fn resolve(&self, e: &Occurrence) -> Result<Option<Occurrence>> {
        let sources: Vec<Occurrence> =
            self.json_rows("SELECT json FROM sources WHERE occurrence=?", [&e.key])?;
        let calendars = self.calendars()?;
        let mut active = sources
            .into_iter()
            .filter(|s| !s.cancelled && calendars.iter().any(|c| c.key == s.calendar && c.enabled))
            .collect::<Vec<_>>();
        active.sort_by_key(|s| format!("{}:{}", s.account, s.provider_id));
        Ok(active
            .iter()
            .find(|s| s.account == e.account && s.provider_id == e.provider_id)
            .cloned()
            .or_else(|| active.into_iter().next()))
    }
    pub fn remove_account(&self, key: &str) -> Result<()> {
        let tx = self.db.unchecked_transaction().map_err(error)?;
        tx.execute(
            "DELETE FROM sources WHERE calendar IN (SELECT key FROM calendars WHERE account=?)",
            [key],
        )
        .map_err(error)?;
        tx.execute("DELETE FROM calendars WHERE account=?", [key])
            .map_err(error)?;
        tx.execute("DELETE FROM accounts WHERE id=?", [key])
            .map_err(error)?;
        tx.execute("DELETE FROM events WHERE NOT EXISTS (SELECT 1 FROM sources WHERE occurrence=events.key)",[]).map_err(error)?;
        tx.commit().map_err(error)
    }
}
