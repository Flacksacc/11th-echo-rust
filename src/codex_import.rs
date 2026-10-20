//! Bounded catalog presentation; source storage and import jobs live in the bridge.
use crate::{CodexConversationRow, CodexImportWindow, CodexPreviewRow};
use serde::Deserialize;
use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default, Deserialize)]
struct Profile {
    id: String,
    name: String,
    path: String,
}
#[derive(Clone, Default, Deserialize)]
struct Conversation {
    id: String,
    title: String,
    path: String,
    cwd: String,
    updated: f64,
}
#[derive(Clone, Default, Deserialize)]
struct Preview {
    role: String,
    text: String,
}
#[derive(Default)]
struct State {
    profiles: Vec<Profile>,
    conversations: Vec<Conversation>,
    preview: Vec<Preview>,
    selected: Option<Conversation>,
    profile: String,
    offset: usize,
    preview_offset: usize,
    more: bool,
    preview_more: bool,
    busy: bool,
    importing: bool,
    cancel: bool,
    status: String,
    revision: u64,
    complete: Option<Value>,
}
pub struct Integration {
    state: Arc<Mutex<State>>,
    _window: CodexImportWindow,
    _timer: slint::Timer,
}
impl Drop for Integration {
    fn drop(&mut self) {
        self.state.lock().unwrap().cancel = true;
    }
}

fn request(
    state: Arc<Mutex<State>>,
    config: crate::hermes::Config,
    operation: String,
    mut args: Value,
) {
    {
        let mut state = state.lock().unwrap();
        if state.busy || state.importing || state.complete.is_some() {
            return;
        }
        state.busy = true;
        state.status = "Loading Codex history…".into();
        if operation == "import" {
            state.importing = true;
            state.cancel = false;
        }
        state.revision += 1;
    }
    std::thread::spawn(move || {
        let result: Result<(), String> = (|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| "Could not start the import worker.")?;
            runtime.block_on(async {
                let client = crate::hermes::Client::new(&config)?;
                let mut op = operation.as_str();
                if matches!(op, "refresh" | "add-profile") {
                    let response = client
                        .codex_import_request(
                            if op == "add-profile" { op } else { "profiles" },
                            args.clone(),
                        )
                        .await?;
                    let profiles: Vec<Profile> =
                        serde_json::from_value(response["profiles"].clone())
                            .map_err(|_| "Invalid profile catalog.")?;
                    let mut locked = state.lock().unwrap();
                    let choice = args["profile_index"].as_u64().unwrap_or_default() as usize;
                    let index = choice.min(profiles.len().saturating_sub(1));
                    locked.profile = profiles
                        .get(index)
                        .map(|p| p.id.clone())
                        .unwrap_or_default();
                    locked.profiles = profiles;
                    args["profile"] = locked.profile.clone().into();
                    args["offset"] = 0.into();
                    op = "catalog";
                    if locked.profile.is_empty() {
                        return Err("No Codex profiles found. Add a Codex home folder.".into());
                    }
                }
                let response = client.codex_import_request(op, args.clone()).await?;
                if op == "import" {
                    let job = response["job"]
                        .as_str()
                        .ok_or("Hermes did not return an import job.")?
                        .to_owned();
                    loop {
                        let cancelled = state.lock().unwrap().cancel;
                        if cancelled {
                            client
                                .codex_import_request("cancel", json!({"job":job}))
                                .await?;
                        }
                        let progress = client
                            .codex_import_request("status", json!({"job":job}))
                            .await?;
                        let status = progress["status"].as_str().unwrap_or("failed");
                        if status == "complete" {
                            state.lock().unwrap().complete = Some(progress);
                            break;
                        }
                        if matches!(status, "cancelled" | "failed") {
                            return Err(progress["error"]
                                .as_str()
                                .unwrap_or("Import failed.")
                                .into());
                        }
                        {
                            let mut locked = state.lock().unwrap();
                            locked.status = format!(
                                "Importing: {} records copied…",
                                progress["records"].as_u64().unwrap_or_default()
                            );
                            locked.revision += 1;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    }
                } else {
                    let mut locked = state.lock().unwrap();
                    if op == "catalog" {
                        locked.conversations =
                            serde_json::from_value(response["conversations"].clone())
                                .map_err(|_| "Invalid conversation catalog.")?;
                        locked.offset = args["offset"].as_u64().unwrap_or_default() as usize;
                        locked.more = response["more"] == true;
                        locked.selected = None;
                        locked.preview.clear();
                        locked.preview_offset = 0;
                        locked.preview_more = false;
                    } else if op == "preview" {
                        locked.preview = serde_json::from_value(response["blocks"].clone())
                            .map_err(|_| "Invalid conversation preview.")?;
                        locked.preview_offset =
                            args["offset"].as_u64().unwrap_or_default() as usize;
                        locked.preview_more = response["more"] == true;
                    }
                }
                Ok(())
            })
        })();
        let mut state = state.lock().unwrap();
        state.busy = false;
        state.importing = false;
        state.status = match result { Ok(()) if state.complete.is_some() => "Import complete. The new tab opens when recording finishes.".into(), Ok(()) => "Select a conversation to preview or import. Tool records are retained as supporting context.".into(), Err(error) => error };
        state.revision += 1;
    });
}

fn selection(state: &State) -> Option<Value> {
    state.selected.as_ref().map(|c| json!({"profile":state.profile,"id":c.id,"title":c.title,"path":c.path,"cwd":c.cwd,"offset":0}))
}

pub fn install(
    chat: &crate::AssistantChatWindow,
    settings: Arc<Mutex<crate::settings::AppSettings>>,
    controller: crate::hermes_ui::Controller,
) -> Result<Integration, slint::PlatformError> {
    let window = CodexImportWindow::new()?;
    let state = Arc::new(Mutex::new(State::default()));
    {
        let state = state.clone();
        window.window().on_close_requested(move || {
            state.lock().unwrap().cancel = true;
            slint::CloseRequestResponse::HideWindow
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let settings = settings.clone();
        chat.on_import_codex(move || {
            if let Some(window) = weak.upgrade() {
                let _ = window.show();
                #[cfg(test)]
                if let (Ok(width), Ok(height)) = (
                    std::env::var("ECHO_GUI_PICKER_WIDTH"),
                    std::env::var("ECHO_GUI_PICKER_HEIGHT"),
                ) {
                    if let (Ok(width), Ok(height)) = (width.parse::<f32>(), height.parse::<f32>()) {
                        let scale = std::env::var("SLINT_SCALE_FACTOR")
                            .ok().and_then(|value| value.parse::<f32>().ok()).unwrap_or(1.25);
                        window.window().set_size(slint::PhysicalSize::new(
                            (width * scale) as u32, (height * scale) as u32,
                        ));
                    }
                }
                request(state.clone(), settings.lock().unwrap().hermes.clone(), "refresh".into(), json!({"profile_index":window.get_profile_index(),"query":window.get_query().as_str(),"archived":window.get_archived()}));
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let settings = settings.clone();
        window.on_refresh(move || { if let Some(w) = weak.upgrade() {
            request(state.clone(), settings.lock().unwrap().hermes.clone(), "refresh".into(), json!({"profile_index":w.get_profile_index(),"query":w.get_query().as_str(),"archived":w.get_archived()}));
        }});
    }
    {
        let state = state.clone();
        let settings = settings.clone();
        window.on_add_profile(move || {
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Choose a Codex home folder")
                .pick_folder()
            {
                request(
                    state.clone(),
                    settings.lock().unwrap().hermes.clone(),
                    "add-profile".into(),
                    json!({"path":path.to_string_lossy(),"profile_index":0}),
                );
            }
        });
    }
    {
        let state = state.clone();
        let settings = settings.clone();
        window.on_choose(move |index| {
            let args = {
                let mut state = state.lock().unwrap();
                if state.busy || state.importing {
                    return;
                }
                state.selected = state.conversations.get(index.max(0) as usize).cloned();
                state.revision += 1;
                selection(&state)
            };
            if let Some(args) = args {
                request(
                    state.clone(),
                    settings.lock().unwrap().hermes.clone(),
                    "preview".into(),
                    args,
                );
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        let settings = settings.clone();
        window.on_page(move |direction| { if let Some(w) = weak.upgrade() {
            let args = { let state = state.lock().unwrap(); json!({"profile":state.profile,"offset":if direction < 0 {state.offset.saturating_sub(40)} else {state.offset + 40},"query":w.get_query().as_str(),"archived":w.get_archived()}) };
            request(state.clone(), settings.lock().unwrap().hermes.clone(), "catalog".into(), args);
        }});
    }
    {
        let state = state.clone();
        let settings = settings.clone();
        window.on_preview_page(move |direction| {
            let args = {
                let state = state.lock().unwrap();
                selection(&state).map(|mut args| {
                    args["offset"] = json!(if direction < 0 {
                        state.preview_offset.saturating_sub(20)
                    } else {
                        state.preview_offset + 20
                    });
                    args
                })
            };
            if let Some(args) = args {
                request(
                    state.clone(),
                    settings.lock().unwrap().hermes.clone(),
                    "preview".into(),
                    args,
                );
            }
        });
    }
    {
        let state = state.clone();
        let settings = settings.clone();
        window.on_import_selected(move || {
            let args = selection(&state.lock().unwrap());
            if let Some(args) = args {
                request(
                    state.clone(),
                    settings.lock().unwrap().hermes.clone(),
                    "import".into(),
                    args,
                );
            }
        });
    }
    {
        let state = state.clone();
        window.on_cancel_import(move || {
            state.lock().unwrap().cancel = true;
        });
    }
    let timer = slint::Timer::default();
    let timer_state = state.clone();
    let chat_weak = chat.as_weak();
    let weak = window.as_weak();
    let mut revision = u64::MAX;
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(100),
        move || {
            let Some(w) = weak.upgrade() else { return };
            let Ok(mut state) = timer_state.try_lock() else {
                return;
            };
            let can_select = chat_weak
                .upgrade()
                .is_some_and(|chat| !chat.get_listening() && chat.get_live_transcript().is_empty());
            if can_select {
                if let Some(complete) = state.complete.take() {
                    controller.import_conversation(complete);
                    let _ = w.hide();
                }
            }
            if revision == state.revision {
                return;
            }
            revision = state.revision;
            w.set_profiles(ModelRc::new(VecModel::from(
                state
                    .profiles
                    .iter()
                    .map(|p| format!("{} · {}", p.name, p.path).into())
                    .collect::<Vec<_>>(),
            )));
            w.set_conversations(ModelRc::new(VecModel::from(
                state
                    .conversations
                    .iter()
                    .map(|c| CodexConversationRow {
                        title: c.title.clone().into(),
                        details: format!(
                            "{} · {}",
                            c.cwd,
                            chrono::DateTime::from_timestamp(c.updated as i64, 0)
                                .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                                .unwrap_or_default()
                        )
                        .into(),
                    })
                    .collect::<Vec<_>>(),
            )));
            w.set_preview(ModelRc::new(VecModel::from(
                state
                    .preview
                    .iter()
                    .map(|p| CodexPreviewRow {
                        role: p.role.clone().into(),
                        text: p.text.clone().into(),
                    })
                    .collect::<Vec<_>>(),
            )));
            w.set_selected_index(
                state
                    .selected
                    .as_ref()
                    .and_then(|selected| {
                        state.conversations.iter().position(|c| c.id == selected.id)
                    })
                    .map(|i| i as i32)
                    .unwrap_or(-1),
            );
            w.set_busy(state.busy || state.complete.is_some());
            w.set_importing(state.importing);
            w.set_status(state.status.clone().into());
            w.set_more(state.more);
            w.set_previous(state.offset > 0);
            w.set_preview_more(state.preview_more);
            w.set_preview_previous(state.preview_offset > 0);
        },
    );
    Ok(Integration {
        state,
        _window: window,
        _timer: timer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;
    #[test]
    #[ignore = "synthetic bridge required; production picker/controller/Skia interactions"]
    fn codex_import_interactive_fixture() {
        let mut settings = crate::settings::AppSettings::default();
        settings.hermes.bridge_port = std::env::var("ECHO_GUI_CODEX_PORT")
            .unwrap()
            .parse()
            .unwrap();
        settings.hermes.token = "synthetic-codex-import-test-token-123456789".into();
        settings.hermes.phrases_enabled = false;
        settings.hermes.enabled = false;
        let chat = crate::AssistantChatWindow::new().unwrap();
        chat.set_hermes_mode(true);
        let integration = crate::hermes_ui::install(&chat, Arc::new(Mutex::new(settings))).unwrap();
        chat.set_hermes_mode(true);
        chat.show().unwrap();
        let weak = chat.as_weak();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(100),
            move || {
                let Some(chat) = weak.upgrade() else { return };
                if chat.get_selected_tab_id().starts_with("api_echo_import_")
                    && !chat.get_loading_history()
                {
                    assert!(chat.get_rich_blocks().row_count() > 0);
                }
            },
        );
        if std::env::var_os("ECHO_GUI_AUTO_CLOSE").is_some() {
            slint::Timer::single_shot(std::time::Duration::from_secs(45), || {
                let _ = slint::quit_event_loop();
            });
        }
        slint::run_event_loop().unwrap();
        drop(integration);
    }
}
