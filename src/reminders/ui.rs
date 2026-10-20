use super::*;
use crate::{
    CalendarAccountRow, CalendarRemindersWindow, CalendarSourceRow, ReminderEventRow,
    ReminderPopover,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc};
#[test]
#[ignore = "Live Skia/MCP calendar UI fixture"]
fn reminders_interactive_fixture() {
    crate::select_ui_backend().unwrap();
    let path = std::env::temp_dir()
        .join(format!(
            "echo-calendar-ui-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
        .join("state.sqlite");
    let service = Service::create(
        &path,
        Arc::new(Mutex::new(crate::settings::AppSettings::default())),
    )
    .unwrap();
    {
        let store = service.store.lock().unwrap();
        store
            .save_calendar(&store::Calendar {
                key: "demo".into(),
                id: "demo".into(),
                account: "demo".into(),
                name: "Demonstration calendar".into(),
                enabled: true,
            })
            .unwrap();
        for i in 0..85 {
            store
                .upsert(&Occurrence {
                    key: format!("event-{i}"),
                    provider_id: format!("id-{i}"),
                    account: "demo".into(),
                    calendar: "demo".into(),
                    series: "weekly".into(),
                    title: format!("Project review {i}"),
                    start: chrono::Utc::now().timestamp() + 3600 + i * 60,
                    end: chrono::Utc::now().timestamp() + 7200 + i * 60,
                    all_day: None,
                    cancelled: false,
                    link: String::new(),
                })
                .unwrap();
        }
        store
            .set_rule(
                "global",
                Rule {
                    spoken: false,
                    ..Rule::default()
                },
            )
            .unwrap();
    }
    let app = crate::AppWindow::new().unwrap();
    let chat = crate::AssistantChatWindow::new().unwrap();
    let integration = install_service(&app, &chat, service.clone()).unwrap();
    app.invoke_open_reminders();
    if std::env::var_os("ECHO_GUI_REMINDER_POPUP").is_some() {
        let first = service.clone();
        slint::Timer::single_shot(std::time::Duration::from_secs(5), move || {
            first.tick(chrono::Utc::now().timestamp()).unwrap();
        });
        slint::Timer::single_shot(std::time::Duration::from_secs(25), move || {
            service.tick(chrono::Utc::now().timestamp() + 3600).unwrap();
        });
    }
    slint::Timer::single_shot(std::time::Duration::from_secs(600), || {
        let _ = slint::quit_event_loop();
    });
    slint::run_event_loop().unwrap();
    drop(integration);
}
pub struct Integration {
    service: Arc<Service>,
    _window: CalendarRemindersWindow,
    _popover: ReminderPopover,
    _timer: slint::Timer,
}
impl Drop for Integration {
    fn drop(&mut self) {
        self.service.stop();
    }
}
fn clock(value: &str) -> Result<u32, String> {
    let (h, m) = value
        .trim()
        .split_once(':')
        .ok_or("Use HH:MM for clock times.")?;
    let h = h.parse::<u32>().map_err(|_| "Invalid hour.")?;
    let m = m.parse::<u32>().map_err(|_| "Invalid minute.")?;
    if h > 23 || m > 59 {
        return Err("Invalid clock time.".into());
    }
    Ok(h * 60 + m)
}
fn format_clock(m: u32) -> String {
    format!("{:02}:{:02}", m / 60, m % 60)
}
fn report(service: &Service, value: Result<Value, String>) {
    if let Err(e) = value {
        *service.speech_error.lock().unwrap() = e;
        service.changed();
    }
}
fn dispatch(service: &Arc<Service>, args: Value) {
    let s = service.clone();
    std::thread::spawn(move || {
        report(&s, s.command(args));
    });
}
fn load_rule(
    w: &CalendarRemindersWindow,
    s: &Arc<Service>,
    scope: &str,
    voices: &Arc<Vec<(String, String)>>,
) {
    w.set_rule_scope(scope.into());
    w.set_rule_loading(true);
    let weak = w.as_weak();
    let s = s.clone();
    let scope = scope.to_owned();
    let voices = voices.clone();
    std::thread::spawn(move || {
        let result = s.rule(&scope);
        let _ = weak.upgrade_in_event_loop(move |w| {
            if w.get_rule_scope().as_str() != scope {
                return;
            }
            w.set_rule_loading(false);
            match result {
                Ok(rule) => {
                    w.set_offsets(
                        rule.offsets
                            .iter()
                            .map(|v| v.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                            .into(),
                    );
                    w.set_repeat_minutes(rule.repeat_minutes as i32);
                    w.set_repeat_enabled(rule.repeat_enabled);
                    w.set_snooze_minutes(rule.snooze_minutes as i32);
                    w.set_all_day_time(format_clock(rule.all_day_minute).into());
                    w.set_enabled(rule.enabled);
                    w.set_spoken(rule.spoken);
                    w.set_quiet(rule.quiet);
                    w.set_quiet_start(format_clock(rule.quiet_start).into());
                    w.set_quiet_end(format_clock(rule.quiet_end).into());
                    w.set_timezone(rule.timezone.into());
                    w.set_voice_index(
                        voices
                            .iter()
                            .position(|(_, id)| id == &rule.voice)
                            .unwrap_or_default() as i32,
                    );
                }
                Err(e) => report(&s, Err(e)),
            }
        });
    });
}
fn update_events(model: &VecModel<ReminderEventRow>, next: Vec<ReminderEventRow>) {
    if model.row_count() != next.len()
        || next
            .iter()
            .enumerate()
            .any(|(i, n)| model.row_data(i).is_none_or(|p| p.id != n.id))
    {
        model.set_vec(next);
        return;
    }
    for (i, n) in next.into_iter().enumerate() {
        if model.row_data(i).is_none_or(|p| {
            p.title != n.title
                || p.detail != n.detail
                || p.status != n.status
                || p.active != n.active
        }) {
            model.set_row_data(i, n);
        }
    }
}
pub fn install(
    app: &crate::AppWindow,
    chat: &crate::AssistantChatWindow,
    settings: Arc<Mutex<crate::settings::AppSettings>>,
) -> Result<Integration, Box<dyn std::error::Error>> {
    install_service(app, chat, Service::start(settings)?)
}
fn install_service(
    app: &crate::AppWindow,
    chat: &crate::AssistantChatWindow,
    service: Arc<Service>,
) -> Result<Integration, Box<dyn std::error::Error>> {
    let window = CalendarRemindersWindow::new()?;
    let popover = ReminderPopover::new()?;
    let mut choices = vec![(
        "Assistant default voice".into(),
        crate::voices::FOLLOW_DEFAULT.into(),
    )];
    for voice in crate::voices::list().unwrap_or_default() {
        choices.push((voice.name.clone(), voice.path().to_string_lossy().into()));
    }
    let voices = Arc::new(choices);
    window.set_voices(ModelRc::new(VecModel::from(
        voices
            .iter()
            .map(|(name, _)| name.clone().into())
            .collect::<Vec<slint::SharedString>>(),
    )));
    let scope = Rc::new(RefCell::new("global".to_string()));
    let selected = Rc::new(RefCell::new(String::new()));
    let offset = Rc::new(RefCell::new(0usize));
    let cache = Arc::new(Mutex::new(Value::Null));
    let ready = Arc::new(Mutex::new(None::<Result<Value, String>>));
    let loading = Arc::new(AtomicBool::new(false));
    let event_model = Rc::new(VecModel::<ReminderEventRow>::default());
    window.set_events(ModelRc::from(event_model.clone()));
    load_rule(&window, &service, "global", &voices);
    {
        let s = service.clone();
        let weak = window.as_weak();
        std::thread::spawn(move || {
            let id = s
                .store
                .lock()
                .unwrap()
                .meta("client_id")
                .unwrap_or_default();
            let _ = weak.upgrade_in_event_loop(move |w| {
                w.set_client_id(if id.is_empty() {
                    option_env!("ECHO_MICROSOFT_CLIENT_ID")
                        .unwrap_or_default()
                        .into()
                } else {
                    id.into()
                })
            });
        });
    }
    for host in [0, 1] {
        let weak = window.as_weak();
        if host == 0 {
            app.on_open_reminders(move || {
                if let Some(w) = weak.upgrade() {
                    let _ = w.show();
                }
            });
        } else {
            chat.on_open_reminders(move || {
                if let Some(w) = weak.upgrade() {
                    let _ = w.show();
                }
            });
        }
    }
    {
        let s = service.clone();
        let weak = window.as_weak();
        window.on_connect(move || {
            if let Some(w) = weak.upgrade() {
                let id = w.get_client_id().trim().to_owned();
                match outlook::validate_client(&id) {
                    Ok(()) => s.connect(id),
                    Err(e) => report(&s, Err(e)),
                }
            }
        });
    }
    {
        let s = service.clone();
        window.on_refresh(move || s.refresh());
    }
    {
        let s = service.clone();
        window.on_disconnect(move |key| {
            dispatch(&s, json!({"action":"remove_account","key":key.as_str()}))
        });
    }
    {
        let s = service.clone();
        window.on_toggle_calendar(move |key, on| {
            let s = s.clone();
            std::thread::spawn(move || {
                report(
                    &s,
                    s.command(json!({"action":"calendar","key":key.as_str(),"enabled":on})),
                );
                s.refresh();
            });
        });
    }
    {
        let pick = selected.clone();
        let weak = window.as_weak();
        window.on_select_event(move |key| {
            *pick.borrow_mut() = key.to_string();
            if let Some(w) = weak.upgrade() {
                if w.get_scope_index() == 0 {
                    w.set_scope_index(3);
                }
                w.invoke_scope_changed();
            }
        });
    }
    {
        let scope = scope.clone();
        let selected = selected.clone();
        let cache = cache.clone();
        let s = service.clone();
        let weak = window.as_weak();
        let voices = voices.clone();
        window.on_scope_changed(move || {
            if let Some(w) = weak.upgrade() {
                let key = selected.borrow().clone();
                let data = cache.lock().unwrap().clone();
                let e = data["events"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|v| v["event"]["key"] == key)
                    .map(|v| v["event"].clone());
                let next = if w.get_scope_index() == 0 {
                    Ok(("global".to_owned(), "Global defaults".to_owned()))
                } else {
                    e.ok_or("Select an event on this page first.".to_string())
                        .and_then(|e| {
                            let title = e["title"].as_str().unwrap_or("Event");
                            let scope = match w.get_scope_index() {
                                1 => format!(
                                    "calendar:{}",
                                    e["calendar"].as_str().unwrap_or_default()
                                ),
                                2 if !e["series"].as_str().unwrap_or_default().is_empty() => {
                                    format!(
                                        "series:{}:{}",
                                        e["account"].as_str().unwrap_or_default(),
                                        e["series"].as_str().unwrap_or_default()
                                    )
                                }
                                3 => {
                                    format!("occurrence:{}", e["key"].as_str().unwrap_or_default())
                                }
                                _ => {
                                    return Err(
                                        "Choose a recurring event to configure its series.".into()
                                    )
                                }
                            };
                            Ok((
                                scope,
                                format!(
                                    "{} · {title}",
                                    match w.get_scope_index() {
                                        1 => "This calendar",
                                        2 => "This recurring series",
                                        _ => "This occurrence",
                                    }
                                ),
                            ))
                        })
                };
                match next {
                    Ok((next, label)) => {
                        *scope.borrow_mut() = next.clone();
                        w.set_scope_title(label.into());
                        load_rule(&w, &s, &next, &voices);
                    }
                    Err(e) => {
                        report(&s, Err(e));
                        w.set_scope_index(0);
                        *scope.borrow_mut() = "global".into();
                        w.set_scope_title("Global defaults".into());
                        load_rule(&w, &s, "global", &voices);
                    }
                }
            }
        });
    }
    {
        let scope = scope.clone();
        let s = service.clone();
        let weak = window.as_weak();
        let voices = voices.clone();
        window.on_save_rule(move || {
            if let Some(w) = weak.upgrade() {
                let rule = (|| {
                    Ok::<_, String>(Rule {
                        enabled: w.get_enabled(),
                        spoken: w.get_spoken(),
                        offsets: w
                            .get_offsets()
                            .split(',')
                            .map(|s| {
                                s.trim().parse::<i64>().map_err(|_| {
                                    "Offsets must be comma-separated whole minutes.".into()
                                })
                            })
                            .collect::<Result<Vec<_>, String>>()?,
                        repeat_enabled: w.get_repeat_enabled(),
                        repeat_minutes: w.get_repeat_minutes() as i64,
                        snooze_minutes: w.get_snooze_minutes() as i64,
                        all_day_minute: clock(&w.get_all_day_time())?,
                        quiet: w.get_quiet(),
                        quiet_start: clock(&w.get_quiet_start())?,
                        quiet_end: clock(&w.get_quiet_end())?,
                        timezone: w.get_timezone().trim().to_owned(),
                        voice: voices
                            .get(w.get_voice_index().max(0) as usize)
                            .map(|(_, id)| id.clone())
                            .unwrap_or_else(|| crate::voices::FOLLOW_DEFAULT.into()),
                    })
                })();
                match rule {
                    Ok(rule) => dispatch(
                        &s,
                        json!({"action":"rule","scope":scope.borrow().as_str(),"rule":rule}),
                    ),
                    Err(e) => report(&s, Err(e)),
                }
            }
        });
    }
    {
        let scope = scope.clone();
        let s = service.clone();
        let weak = window.as_weak();
        let voices = voices.clone();
        window.on_inherit_rule(move || {
            let scope = scope.borrow().clone();
            let s = s.clone();
            let weak = weak.clone();
            let voices = voices.clone();
            std::thread::spawn(move || {
                report(&s, s.command(json!({"action":"inherit","scope":scope})));
                let _ = weak.upgrade_in_event_loop(move |w| load_rule(&w, &s, &scope, &voices));
            });
        });
    }
    {
        let offset = offset.clone();
        let s = service.clone();
        window.on_page(move |direction| {
            let previous = *offset.borrow();
            *offset.borrow_mut() = if direction < 0 {
                previous.saturating_sub(40)
            } else {
                previous + 40
            };
            s.changed();
        });
    }
    for action in ["dismiss", "snooze", "enable"] {
        let s = service.clone();
        let callback = move |key: slint::SharedString| {
            dispatch(&s, json!({"action":action,"key":key.as_str()}))
        };
        match action {
            "dismiss" => window.on_dismiss(callback),
            "snooze" => window.on_snooze(callback),
            _ => window.on_enable_event(callback),
        }
    }
    {
        let s = service.clone();
        popover.on_dismiss(move || {
            if let Some(n) = s.visible_notice() {
                dispatch(&s, json!({"action":"dismiss","key":n.occurrence.key}));
            }
        });
    }
    {
        let s = service.clone();
        popover.on_snooze(move || {
            if let Some(n) = s.visible_notice() {
                dispatch(&s, json!({"action":"snooze","key":n.occurrence.key}));
            }
        });
    }
    {
        let weak = window.as_weak();
        popover.on_open_reminders(move || {
            if let Some(w) = weak.upgrade() {
                let _ = w.show();
            }
        });
    }
    let timer = slint::Timer::default();
    let weak = window.as_weak();
    let popup = popover.as_weak();
    let s = service.clone();
    let mut requested = 0;
    let mut last_display = String::new();
    timer.start(slint::TimerMode::Repeated,std::time::Duration::from_millis(200),move||{
        let Some(w)=weak.upgrade()else{return;};let r=s.revision.load(Ordering::SeqCst);
        if requested!=r&&!loading.swap(true,Ordering::SeqCst){requested=r;let s=s.clone();let ready=ready.clone();let loading=loading.clone();let offset=*offset.borrow();std::thread::spawn(move||{*ready.lock().unwrap()=Some(s.snapshot(offset));loading.store(false,Ordering::SeqCst);});}
        let data=ready.lock().unwrap().take();if let Some(result)=data{match result{Ok(data)=>{
            *cache.lock().unwrap()=data.clone();
            w.set_accounts(ModelRc::new(VecModel::from(data["accounts"].as_array().into_iter().flatten().map(|a|CalendarAccountRow{id:a["id"].as_str().unwrap_or_default().into(),name:a["name"].as_str().unwrap_or_default().into(),status:a["status"].as_str().unwrap_or_default().into()}).collect::<Vec<_>>())));
            w.set_calendars(ModelRc::new(VecModel::from(data["calendars"].as_array().into_iter().flatten().map(|c|CalendarSourceRow{id:c["key"].as_str().unwrap_or_default().into(),name:c["name"].as_str().unwrap_or_default().into(),enabled:c["enabled"]==true}).collect::<Vec<_>>())));
            update_events(&event_model,data["events"].as_array().into_iter().flatten().map(|v|{
                let e=&v["event"];let p=&v["progress"];let active=p["activated"]==true&&p["dismissed"]!=true&&e["cancelled"]!=true&&v["enabled"]==true;
                let status=if p["dismissed"]==true{"Dismissed"}else if e["cancelled"]==true{"Cancelled"}else if v["enabled"]!=true{"Disabled"}else if p["snooze_until"].as_i64().unwrap_or_default()>chrono::Utc::now().timestamp(){"Snoozed"}else if active{"Awaiting acknowledgment"}else{"Scheduled"};
                ReminderEventRow{id:e["key"].as_str().unwrap_or_default().into(),title:e["title"].as_str().unwrap_or_default().into(),detail:chrono::DateTime::from_timestamp(e["start"].as_i64().unwrap_or_default(),0).map(|d|d.with_timezone(&chrono::Local).format("%a %b %d · %H:%M").to_string()).unwrap_or_default().into(),status:status.into(),active}
            }).collect());w.set_more(data["more"]==true);w.set_previous(*offset.borrow()>0);w.set_busy(data["busy"]==true);
            w.set_status(if !data["error"].as_str().unwrap_or_default().is_empty(){data["error"].as_str().unwrap_or_default().into()}else if data["busy"]==true{"Connecting / synchronizing Microsoft calendars…".into()}else{"Reminders work while Echo runs in the tray. Say “stop reminding me” to dismiss the current occurrence.".into()});
        },Err(e)=>report(&s,Err(e))}}
        if let Some(p)=popup.upgrade(){if let Some(notice)=s.visible_notice(){let ticket=format!("{}:{}",notice.occurrence.key,notice.ticket);
            if last_display!=ticket{p.set_text(notice.text.into());p.set_detail("Voice follows the current sentence · dismiss stops this occurrence".into());last_display=ticket;
                if !crate::assistant_presentation::runtime_snapshot().avatar.visible{show_without_activation(&p);}}
            configure_no_activation(p.window());
        }else{last_display.clear();let _=p.hide();}}
    });
    Ok(Integration {
        service,
        _window: window,
        _popover: popover,
        _timer: timer,
    })
}
#[cfg(windows)]
fn show_without_activation(popover: &ReminderPopover) {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    use windows::Win32::{
        Foundation::{LPARAM, LRESULT, WPARAM},
        System::Threading::GetCurrentThreadId,
        UI::WindowsAndMessaging::{
            CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx, HCBT_ACTIVATE, WH_CBT,
        },
    };
    // Slint creates/recreates the HWND during show. Block activation for this
    // synchronous show only; then persist NOACTIVATE on the actual new HWND.
    unsafe extern "system" fn no_activation(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
        if code == HCBT_ACTIVATE as i32 {
            return LRESULT(1);
        }
        unsafe { CallNextHookEx(None, code, w, l) }
    }
    let hook =
        unsafe { SetWindowsHookExW(WH_CBT, Some(no_activation), None, GetCurrentThreadId()) }.ok();
    let _ = popover.show();
    if let Some(hook) = hook {
        unsafe {
            let _ = UnhookWindowsHookEx(hook);
        }
    }
    let window = popover.window();
    let size = window.size();
    unsafe {
        window.set_position(slint::PhysicalPosition::new(
            (GetSystemMetrics(SM_CXSCREEN) - size.width as i32 - 24).max(0),
            (GetSystemMetrics(SM_CYSCREEN) - size.height as i32 - 48).max(0),
        ));
    }
    configure_no_activation(window);
}
#[cfg(windows)]
fn configure_no_activation(window: &slint::Window) {
    use raw_window_handle::HasWindowHandle;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    };
    let handle = window.window_handle();
    if let Ok(handle) = handle.window_handle() {
        if let raw_window_handle::RawWindowHandle::Win32(handle) = handle.as_raw() {
            let hwnd = windows::Win32::Foundation::HWND(handle.hwnd.get());
            unsafe {
                let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                if style & (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) as isize
                    != (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) as isize
                {
                    SetWindowLongPtrW(
                        hwnd,
                        GWL_EXSTYLE,
                        style | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize,
                    );
                }
            }
        }
    }
}
#[cfg(not(windows))]
fn configure_no_activation(_: &slint::Window) {}
#[cfg(not(windows))]
fn show_without_activation(popover: &ReminderPopover) {
    let _ = popover.show();
}
