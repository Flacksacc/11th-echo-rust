//! Sentence-boundary arbitration; entering a lane never cancels agent work.
use std::sync::{Condvar, Mutex};
#[derive(Default)]
struct State {
    active: bool,
    reminders: usize,
    waiting: usize,
}
#[derive(Default)]
pub struct Lane {
    state: Mutex<State>,
    ready: Condvar,
}
pub struct Permit<'a>(&'a Lane);
impl Lane {
    pub fn enter(&self, priority: bool, cancel: impl Fn() -> bool) -> Option<Permit<'_>> {
        let mut state = self.state.lock().unwrap();
        state.waiting += 1;
        if priority {
            state.reminders += 1;
        }
        loop {
            if cancel() {
                state.waiting -= 1;
                if priority {
                    state.reminders -= 1;
                }
                self.ready.notify_all();
                return None;
            }
            if !state.active && (priority || state.reminders == 0) {
                state.waiting -= 1;
                if priority {
                    state.reminders -= 1;
                }
                state.active = true;
                return Some(Permit(self));
            }
            state = self
                .ready
                .wait_timeout(state, std::time::Duration::from_millis(50))
                .unwrap()
                .0;
        }
    }
    pub fn try_enter(&self) -> Option<Permit<'_>> {
        let mut state = self.state.lock().unwrap();
        if state.active || state.reminders > 0 || state.waiting > 0 {
            return None;
        }
        state.active = true;
        Some(Permit(self))
    }
    pub fn has_waiters(&self) -> bool {
        self.state.lock().unwrap().waiting > 0
    }
}
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().active = false;
        self.0.ready.notify_all();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn reminder_waits_for_sentence_then_reply_continues() {
        let lane = Arc::new(Lane::default());
        let first = lane.enter(false, || false).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let l = lane.clone();
        let e = events.clone();
        let reminder = std::thread::spawn(move || {
            let _p = l.enter(true, || false).unwrap();
            e.lock().unwrap().push("reminder");
        });
        while lane.state.lock().unwrap().reminders == 0 {
            std::thread::yield_now();
        }
        let l = lane.clone();
        let e = events.clone();
        let next = std::thread::spawn(move || {
            let _p = l.enter(false, || false).unwrap();
            e.lock().unwrap().push("next sentence");
        });
        assert!(events.lock().unwrap().is_empty());
        drop(first);
        reminder.join().unwrap();
        next.join().unwrap();
        assert_eq!(*events.lock().unwrap(), ["reminder", "next sentence"]);
    }
    #[test]
    fn cancelled_reminder_does_not_block_reply() {
        let lane = Lane::default();
        assert!(lane.enter(true, || true).is_none());
        assert!(lane.try_enter().is_some());
    }
}
