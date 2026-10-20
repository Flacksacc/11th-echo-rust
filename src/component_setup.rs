use crate::{
    components,
    settings::{save_settings, AppSettings},
    ComponentChoice, ComponentSetupWindow,
};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

fn populate(window: &ComponentSetupWindow, choices: &[components::Choice]) {
    let page = window.get_page();
    window.set_choices(ModelRc::new(VecModel::from(
        choices
            .iter()
            .filter(|c| c.page == page || page == 5 && c.selected)
            .map(|c| ComponentChoice {
                id: c.id.clone().into(),
                title: c.title.clone().into(),
                description: c.description.clone().into(),
                tradeoff: c.tradeoff.clone().into(),
                download: c.download.clone().into(),
                selected: c.selected,
                installed: c.installed,
            })
            .collect::<Vec<_>>(),
    )));
}

pub fn show(shared: Arc<Mutex<AppSettings>>) -> Result<ComponentSetupWindow, slint::PlatformError> {
    let window = ComponentSetupWindow::new()?;
    let mut entries = components::catalog();
    let snapshot = shared.lock().unwrap().clone();
    for choice in &mut entries {
        choice.installed = snapshot.installed_components.contains(&choice.id)
            || components::downloaded(&choice.id);
    }
    if let Some(selected) = &snapshot.component_selection {
        for choice in &mut entries {
            choice.selected = selected.contains(&choice.id);
        }
    }
    let catalog = Rc::new(RefCell::new(entries));
    window.set_brave_key(snapshot.brave_api_key.into());
    window.set_hf_token(snapshot.hugging_face_token.into());
    populate(&window, &catalog.borrow());
    let weak = window.as_weak();
    let choices = catalog.clone();
    let page_settings = shared.clone();
    window.on_change_page(move |page| {
        if let Some(window) = weak.upgrade() {
            window.set_page(page.clamp(0, 5));
            let installed = page_settings.lock().unwrap().installed_components.clone();
            for choice in choices.borrow_mut().iter_mut() {
                choice.installed =
                    installed.contains(&choice.id) || components::downloaded(&choice.id);
            }
            populate(&window, &choices.borrow());
        }
    });
    let choices = catalog.clone();
    let weak = window.as_weak();
    window.on_select_component(move |id, selected| {
        if let Some(choice) = choices
            .borrow_mut()
            .iter_mut()
            .find(|c| c.id == id.as_str())
        {
            choice.selected = selected;
        }
        if let Some(window) = weak.upgrade() {
            populate(&window, &choices.borrow());
        }
    });
    let host = crate::AppWindow::new()?;
    crate::model_ui::install(&host, shared.clone());
    window.on_configure_models(move || host.invoke_open_model_manager());
    let cancelled = Arc::new(AtomicBool::new(false));
    let close_cancel = cancelled.clone();
    window.window().on_close_requested(move || {
        close_cancel.store(true, Ordering::SeqCst);
        slint::CloseRequestResponse::HideWindow
    });
    let close_cancel = cancelled.clone();
    let weak = window.as_weak();
    window.on_close_setup(move || {
        close_cancel.store(true, Ordering::SeqCst);
        if let Some(window) = weak.upgrade() {
            let _ = window.hide();
        }
    });
    let weak = window.as_weak();
    let choices = catalog;
    let settings = shared.clone();
    window.on_install_selected(move || {
        let Some(window) = weak.upgrade() else { return; };
        let Some(activity) = crate::post_processing::ActivityGuard::acquire() else {
            window.set_status("Finish recording or the current model installation first.".into()); return;
        };
        cancelled.store(false, Ordering::SeqCst);
        let ids = choices.borrow().iter().filter(|c| c.selected).map(|c| c.id.clone()).collect::<Vec<_>>();
        let mut snapshot = settings.lock().unwrap().clone();
        snapshot.brave_api_key = window.get_brave_key().to_string();
        snapshot.hugging_face_token = window.get_hf_token().to_string();
        snapshot.component_selection = Some(ids.clone());
        if !save_settings(&snapshot) { window.set_status("Could not protect or save settings.".into()); return; }
        *settings.lock().unwrap() = snapshot;
        window.set_installing(true); window.set_status("Preparing installation…".into());
        let weak = weak.clone(); let shared = settings.clone(); let cancel = cancelled.clone();
        std::thread::spawn(move || {
            let _activity = activity;
            let result = tokio::runtime::Runtime::new().map_err(|_| "Could not start installer.".to_string())
                .and_then(|runtime| runtime.block_on(async {
                    let mut failures = vec![];
                    for id in &ids {
                        if id == "pocket" && ids.iter().any(|id| id == "pocket_clone") { continue; }
                        let ui = weak.clone(); let progress: components::Progress = Arc::new(move |status| {
                            let _ = ui.upgrade_in_event_loop(move |window| window.set_status(status.into()));
                        });
                        let token = shared.lock().unwrap().hugging_face_token.clone();
                        let installed = tokio::select! {
                            result = components::install(id, &token, progress) => result,
                            _ = async { while !cancel.load(Ordering::SeqCst) { tokio::time::sleep(std::time::Duration::from_millis(50)).await; } } => return Err("Installation cancelled. Completed components are retained.".into())
                        };
                        if let Err(error) = installed { failures.push(format!("{id}: {error}")); continue; }
                        let connection = async {
                            if id == "desktop" || id == "browser" {
                                let mut server = components::server(id)?;
                                let client = crate::assistant_mcp::connect(&server).await?;
                                server.discovered_tools = tokio::time::timeout(std::time::Duration::from_secs(15), client.peer().list_all_tools()).await
                                    .map_err(|_| "Tool discovery timed out.")?.map_err(|_| "Installed tool discovery failed.")?;
                                let _ = client.cancel().await;
                                let mut settings = shared.lock().unwrap();
                                if let Some(existing) = settings.assistant_mcp_servers.iter_mut().find(|s| s.id == server.id) {
                                    if existing.transport != server.transport || existing.environment != server.environment || existing.headers != server.headers {
                                        return Err("Existing computer-tool connection was customized; its configuration was retained. Update it in Agent tools if needed.".into());
                                    }
                                    existing.discovered_tools = server.discovered_tools;
                                }
                                else { settings.assistant_mcp_servers.push(server); }
                            }
                            if id == "laya" { components::register_laya(&mut shared.lock().unwrap())?; }
                            Ok::<_, String>(())
                        }.await;
                        if let Err(error) = connection { failures.push(format!("{id}: {error}")); }
                        let mut settings = shared.lock().unwrap();
                        if id == "pocket" { settings.installed_components.retain(|c| c != "pocket_clone"); }
                        if id == "pocket_clone" { settings.installed_components.retain(|c| c != "pocket"); }
                        if !settings.installed_components.contains(id) { settings.installed_components.push(id.clone()); }
                        if !save_settings(&settings) { failures.push("Could not save component status.".into()); }
                    }
                    if failures.is_empty() { Ok("Selected components are ready. Close setup to continue.".to_string()) }
                    else { Err(format!("Some components need retrying:\n{}", failures.join("\n"))) }
                }));
            let text = result.unwrap_or_else(|e| e);
            let _ = weak.upgrade_in_event_loop(move |window| { window.set_status(text.into()); window.set_installing(false);window.invoke_change_page(window.get_page()); });
        });
    });
    let weak = window.as_weak();
    window.on_remove_component(move |id| {
        let Some(window) = weak.upgrade() else { return; };
        let Some(_activity) = crate::post_processing::ActivityGuard::acquire() else {
            window.set_status("Finish recording or installation first.".into()); return;
        };
        if rfd::MessageDialog::new().set_title("Remove downloaded component?")
            .set_description("Graphs that use this component will need it reinstalled. Your graphs and settings will be retained.")
            .set_buttons(rfd::MessageButtons::YesNo).show() == rfd::MessageDialogResult::Yes {
            let result = components::remove(&id).and_then(|()| {
                let mut next = shared.lock().unwrap().clone(); next.installed_components.retain(|c| c != id.as_str() && !(matches!(id.as_str(), "pocket" | "pocket_clone") && matches!(c.as_str(), "pocket" | "pocket_clone")));
                if save_settings(&next) { *shared.lock().unwrap() = next; Ok(()) } else { Err("Removed, but could not save component status.".into()) }
            });
            window.set_status(result.map(|_| "Component removed.".into()).unwrap_or_else(|error| error).into());
            window.invoke_change_page(window.get_page());
        }
    });
    window.show()?;
    Ok(window)
}
