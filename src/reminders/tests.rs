use super::*;
fn fixture() -> (Arc<Service>, std::path::PathBuf) {
    let path = std::env::temp_dir()
        .join(format!(
            "echo-reminders-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
        .join("state.sqlite");
    let s = Service::create(
        &path,
        Arc::new(Mutex::new(crate::settings::AppSettings::default())),
    )
    .unwrap();
    let store = s.store.lock().unwrap();
    store
        .save_calendar(&store::Calendar {
            key: "c".into(),
            id: "c".into(),
            account: "a".into(),
            name: "Calendar".into(),
            enabled: true,
        })
        .unwrap();
    store.upsert(&event()).unwrap();
    drop(store);
    (s, path)
}
fn event() -> Occurrence {
    Occurrence {
        key: "e".into(),
        provider_id: "p".into(),
        account: "a".into(),
        calendar: "c".into(),
        series: "s".into(),
        title: "Review".into(),
        start: 100000,
        end: 103600,
        all_day: None,
        cancelled: false,
        link: String::new(),
    }
}
#[test]
fn dismissal_survives_sync_and_restart_and_does_not_dismiss_series() {
    let (s, path) = fixture();
    s.tick(96400).unwrap();
    assert!(s.visible_notice().is_some());
    s.command(json!({"action":"dismiss","key":"e"})).unwrap();
    let mut next = event();
    next.key = "next".into();
    next.provider_id = "next".into();
    next.start += 86400;
    next.end += 86400;
    {
        let store = s.store.lock().unwrap();
        store.upsert(&event()).unwrap();
        store.upsert(&next).unwrap();
    }
    drop(s);
    let s = Service::create(
        &path,
        Arc::new(Mutex::new(crate::settings::AppSettings::default())),
    )
    .unwrap();
    s.tick(next.start - 3600).unwrap();
    let store = s.store.lock().unwrap();
    assert!(store.event("e").unwrap().1.dismissed);
    assert!(store.event("next").unwrap().1.activated);
}
#[test]
fn partial_rules_inherit_and_validate_without_losing_defaults() {
    let (s, _) = fixture();
    s.command(json!({"action":"rule","scope":"global","rule":{"repeat_minutes":7}}))
        .unwrap();
    s.command(json!({"action":"rule","scope":"calendar:c","rule":{"spoken":false}}))
        .unwrap();
    assert_eq!(s.rule("occurrence:e").unwrap().repeat_minutes, 7);
    assert!(!s.rule("occurrence:e").unwrap().spoken);
    s.command(json!({"action":"rule","scope":"series:a:s","rule":{"repeat_minutes":9}}))
        .unwrap();
    s.command(json!({"action":"rule","scope":"occurrence:e","rule":{"repeat_minutes":11}}))
        .unwrap();
    assert_eq!(s.rule("occurrence:e").unwrap().repeat_minutes, 11);
    s.command(json!({"action":"inherit","scope":"occurrence:e"}))
        .unwrap();
    assert_eq!(s.rule("occurrence:e").unwrap().repeat_minutes, 9);
    assert!(s
        .command(json!({"action":"rule","rule":{"all_day_minute":99999}}))
        .is_err());
    assert!(s
        .command(json!({"action":"rule","rule":{"unknown":true}}))
        .is_err());
}
#[test]
fn duplicated_calendars_cancel_only_when_all_sources_inactive() {
    let (s, _) = fixture();
    let store = s.store.lock().unwrap();
    let mut alias = event();
    alias.calendar = "other".into();
    alias.provider_id = "alias".into();
    store
        .save_calendar(&store::Calendar {
            key: "other".into(),
            id: "other".into(),
            account: "a".into(),
            name: "Other".into(),
            enabled: true,
        })
        .unwrap();
    store.upsert(&alias).unwrap();
    let mut original = event();
    original.cancelled = true;
    store.upsert(&original).unwrap();
    assert!(store.resolve(&original).unwrap().is_some());
    alias.cancelled = true;
    store.upsert(&alias).unwrap();
    assert!(store.resolve(&original).unwrap().is_none());
    assert_eq!(store.events(0, 41).unwrap().len(), 1);
}
#[test]
fn snooze_restoration_cancellation_and_repeat_off() {
    let (s, _) = fixture();
    s.tick(100000).unwrap();
    let n = s.pending.lock().unwrap()["e"].clone();
    assert!(s.eligible(&n));
    s.command(json!({"action":"snooze","key":"e","minutes":10}))
        .unwrap();
    assert!(!s.eligible(&n));
    assert!(s.visible_notice().is_none());
    let mut p = s.store.lock().unwrap().event("e").unwrap().1;
    p.snooze_until = 100600;
    s.store.lock().unwrap().progress("e", &p).unwrap();
    s.tick(100600).unwrap();
    let n = s.pending.lock().unwrap()["e"].clone();
    assert!(n.ticket.contains("snooze:"));
    s.tick(100601).unwrap();
    assert_eq!(s.pending.lock().unwrap()["e"].ticket, n.ticket);
    let mut e = event();
    e.cancelled = true;
    s.store.lock().unwrap().upsert(&e).unwrap();
    s.tick(100602).unwrap();
    assert!(!s.eligible(&n));
    let r = Rule {
        repeat_enabled: false,
        ..Rule::default()
    };
    let first = due(&event(), &Progress::default(), &r, 100000).unwrap();
    let p = Progress {
        activated: true,
        last_ticket: first.ticket,
        ..Progress::default()
    };
    assert!(due(&event(), &p, &r, 109000).is_none());
}
#[test]
fn transaction_rollback_and_future_database_protection() {
    let (s, path) = fixture();
    {
        let store = s.store.lock().unwrap();
        let tx = store.db.unchecked_transaction().unwrap();
        let mut e = event();
        e.title = "Changed".into();
        store.upsert(&e).unwrap();
        tx.rollback().unwrap();
        assert_eq!(store.event("e").unwrap().0.title, "Review");
        store.db.execute_batch("PRAGMA user_version=2").unwrap();
    }
    drop(s);
    assert!(Store::open(&path).is_err());
}
#[test]
fn audio_eligibility_does_not_wait_for_sqlite() {
    let (s, _) = fixture();
    s.tick(100000).unwrap();
    let n = s.pending.lock().unwrap()["e"].clone();
    let _held = s.store.lock().unwrap();
    assert!(s.eligible(&n));
    assert!(s.visible_notice().is_some());
}
#[test]
fn rescheduling_rearms_and_account_removal_clears_events() {
    let (s, _) = fixture();
    s.tick(96400).unwrap();
    let mut e = event();
    e.start += 86400;
    e.end += 86400;
    s.store.lock().unwrap().upsert(&e).unwrap();
    s.tick(96401).unwrap();
    assert!(s.visible_notice().is_none());
    assert!(!s.store.lock().unwrap().event("e").unwrap().1.activated);
    s.tick(e.start - 3600).unwrap();
    assert!(s.visible_notice().unwrap().text.contains("60 minutes"));
    s.command(json!({"action":"remove_account","key":"a"}))
        .unwrap();
    assert!(s.store.lock().unwrap().events(0, 41).unwrap().is_empty());
}
#[test]
fn active_old_reminders_restore_but_future_scan_is_bounded() {
    let (s, path) = fixture();
    s.tick(96400).unwrap();
    drop(s);
    let s = Service::create(
        &path,
        Arc::new(Mutex::new(crate::settings::AppSettings::default())),
    )
    .unwrap();
    s.tick(96401).unwrap();
    assert_eq!(s.pending.lock().unwrap().len(), 1);
    let store = s.store.lock().unwrap();
    let mut future = event();
    future.key = "future".into();
    future.provider_id = "future".into();
    future.start += 20 * 86400;
    future.end += 20 * 86400;
    store.upsert(&future).unwrap();
    assert_eq!(store.candidates(96401, 0, 128).unwrap().len(), 1);
    store
        .set_rule(
            "global",
            Rule {
                offsets: vec![30 * 1440, 0],
                ..Rule::default()
            },
        )
        .unwrap();
    assert_eq!(store.candidates(96401, 0, 128).unwrap().len(), 2);
}
