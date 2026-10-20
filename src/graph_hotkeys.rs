//! Transactional per-graph global shortcut registration.
use global_hotkey::{hotkey::HotKey, GlobalHotKeyManager};
pub(crate) fn capture_registered_combo(
    event: u32,
    dictation: Option<u32>,
    assistant: Option<u32>,
    settings: &crate::settings::AppSettings,
) -> Option<String> {
    if dictation == Some(event) {
        return Some(settings.hotkey_text.clone());
    }
    if assistant == Some(event) {
        return Some(settings.assistant_hotkey_text.clone());
    }
    crate::graph_ui::hotkey_specs(settings)
        .into_iter()
        .find(|(_, text)| crate::parse_hotkey(text).is_ok_and(|key| key.id() == event))
        .map(|(_, text)| text)
}
use std::collections::HashMap;

#[derive(Default)]
pub struct GraphHotkeys {
    pub registered: HashMap<String, HotKey>,
}
impl GraphHotkeys {
    pub fn sync(
        &mut self,
        manager: &GlobalHotKeyManager,
        specifications: &[(String, String)],
    ) -> Result<(), String> {
        self.sync_with(
            specifications,
            |key| manager.register(key).map_err(|e| e.to_string()),
            |key| manager.unregister(key).map_err(|e| e.to_string()),
        )
    }
    fn sync_with(
        &mut self,
        specifications: &[(String, String)],
        mut register: impl FnMut(HotKey) -> Result<(), String>,
        mut unregister: impl FnMut(HotKey) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut requested = HashMap::new();
        let mut ids = std::collections::HashSet::new();
        for (id, text) in specifications {
            if text.trim().is_empty() {
                continue;
            }
            let hotkey = crate::parse_hotkey(text)?;
            if !ids.insert(hotkey.id()) {
                return Err("Each graph hotkey must be unique.".into());
            }
            requested.insert(id.clone(), hotkey);
        }
        let removed: Vec<_> = self
            .registered
            .iter()
            .filter(|(id, key)| requested.get(*id) != Some(*key))
            .map(|(id, key)| (id.clone(), *key))
            .collect();
        let added: Vec<_> = requested
            .iter()
            .filter(|(id, key)| self.registered.get(*id) != Some(*key))
            .map(|(id, key)| (id.clone(), *key))
            .collect();
        let mut unregistered = Vec::new();
        for (id, key) in &removed {
            if let Err(error) = unregister(*key) {
                let rollback = self.restore(&unregistered, &mut register);
                return Err(format!(
                    "Could not release graph shortcut: {error}{rollback}"
                ));
            }
            unregistered.push((id.clone(), *key));
        }
        let mut installed: Vec<(String, HotKey)> = Vec::new();
        for (id, key) in &added {
            if let Err(error) = register(*key) {
                let mut cleanup = String::new();
                for (id, new) in &installed {
                    if let Err(error) = unregister(*new) {
                        self.registered.insert(id.clone(), *new);
                        cleanup.push_str(&format!("; shortcut cleanup failed: {error}"));
                    }
                }
                let rollback = self.restore(&removed, &mut register);
                return Err(format!(
                    "Graph shortcut is unavailable: {error}{cleanup}{rollback}"
                ));
            }
            installed.push((id.clone(), *key));
        }
        self.registered = requested;
        Ok(())
    }
    fn restore(
        &mut self,
        removed: &[(String, HotKey)],
        register: &mut impl FnMut(HotKey) -> Result<(), String>,
    ) -> String {
        let mut errors = String::new();
        for (id, key) in removed {
            if let Err(error) = register(*key) {
                self.registered.remove(id);
                errors.push_str(&format!("; restoring {id} failed: {error}"));
            }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registered_shortcuts_capture_their_existing_text() {
        let mut settings = crate::settings::AppSettings::default();
        settings.pipelines = Some(crate::pipelines::migrate(&settings));
        let key = "Ctrl+Shift+F9";
        settings.pipelines.as_mut().unwrap().pipelines[0].graph = Some(crate::graph::migrate(
            &settings.pipelines.as_ref().unwrap().pipelines[0],
            &settings,
        ));
        settings.pipelines.as_mut().unwrap().pipelines[0]
            .graph
            .as_mut()
            .unwrap()
            .hotkey = key.into();
        let id = crate::parse_hotkey(key).unwrap().id();
        assert_eq!(
            capture_registered_combo(id, None, None, &settings),
            Some(key.into())
        );
        assert_eq!(
            capture_registered_combo(7, Some(7), None, &settings),
            Some(settings.hotkey_text.clone())
        );
        assert_eq!(
            capture_registered_combo(8, None, Some(8), &settings),
            Some(settings.assistant_hotkey_text.clone())
        );
        assert!(capture_registered_combo(9, None, None, &settings).is_none());
    }
    #[test]
    fn graph_shortcut_failure_restores_previous_registration() {
        use std::{cell::RefCell, collections::HashSet};
        let keys = RefCell::new(HashSet::new());
        let busy = crate::parse_hotkey("Ctrl+Shift+F12").unwrap().id();
        let mut registry = GraphHotkeys::default();
        let register = |key: HotKey| {
            if key.id() == busy {
                return Err("Occupied by another application".into());
            }
            if keys.borrow_mut().insert(key.id()) {
                Ok(())
            } else {
                Err("Already registered".into())
            }
        };
        let unregister = |key: HotKey| {
            keys.borrow_mut().remove(&key.id());
            Ok(())
        };
        registry
            .sync_with(
                &[("graph".into(), "Ctrl+Shift+F11".into())],
                register,
                unregister,
            )
            .unwrap();
        let old = registry.registered["graph"].id();
        assert!(registry
            .sync_with(
                &[("graph".into(), "Ctrl+Shift+F12".into())],
                register,
                unregister
            )
            .is_err());
        assert_eq!(registry.registered["graph"].id(), old);
        assert!(keys.borrow().contains(&old));
        assert!(!keys.borrow().contains(&busy));
        registry.sync_with(&[], register, unregister).unwrap();
        assert!(keys.borrow().is_empty());
    }
}
