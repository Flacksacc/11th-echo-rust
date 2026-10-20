# Calendar & Reminders

Open **Settings → Reminders**, or **Calendar & reminders…** in the assistant gear menu. Enter your public Microsoft application ID, connect your Microsoft account, and enable the calendars you want. Calendar access is read-only. Echo never edits Outlook events.

The default schedule is 60, 15, and 5 minutes before a meeting, at its start, then every 5 minutes until you dismiss that occurrence. Repetition continues after the meeting ends. All-day events begin at 09:00 local time. Snoozing defaults to 10 minutes. Quiet hours are initially off and suppress speech while keeping reminder cards available.

Select an event to configure that occurrence, its recurring series, or its calendar. Settings inherit in this order: occurrence, series, calendar, global defaults. Use **Inherit defaults** to remove an override. Offsets support up to 30 days; Echo synchronizes the previous day and next 30 days every minute. Event lists are paged, and visible rows are virtualized.

Say **stop reminding me**, **dismiss this reminder**, or **snooze this reminder** while a reminder is showing. Dismissal affects one occurrence, not the whole series. Hermes also exposes `echo_reminders` for listing events, choosing a particular reminder, and changing local reminder rules. Those actions do not stop background work.

Reminder speech waits for the current assistant sentence to finish, then the assistant reply continues. Starting dictation silences/defer speech. Notifications appear on the avatar or in a small reminder popup when the avatar is hidden. Outstanding reminders remain in the event list. Voice defaults to the assistant voice and can be overridden. Echo warms a bounded audio cache when speech is idle; uncached notices use streaming Pocket TTS.

Dismissal, snoozing, rule overrides, calendar choices, and pending deliveries are stored locally in SQLite beside Echo's settings. Microsoft refresh tokens are protected with Windows DPAPI; access tokens stay in memory. Removing an account disconnects it and removes its local calendar data. Echo must be running, including in the tray, to notify you. It installs no background service. After a restart or sleep, missed phases collapse into one current reminder; newly discovered historical meetings do not start nagging.

## Microsoft application registration

The application ID can be supplied later; setup and local reminder configuration work without it. Live calendar synchronization needs a registered Microsoft public desktop application:

1. In Microsoft Entra's application registrations, use your existing application or create one. Choose the account types you intend to support (organizational and personal Microsoft accounts for both).
2. Add a **Mobile and desktop applications** redirect URI of `http://localhost`. Echo uses a temporary loopback port, authorization code flow, and PKCE.
3. Allow delegated Microsoft Graph **Calendars.Read** access. Echo also requests `openid`, `profile`, and `offline_access` during sign-in. Some organizations require administrator consent.
4. Copy the **Application (client) ID** into Echo. No application secret is needed or accepted.
5. Connect the account in Echo's browser flow and choose your calendars.

Registration reference: https://learn.microsoft.com/en-us/entra/identity-platform/scenario-desktop-app-registration

Live sign-in and Microsoft account behavior require account-specific verification. Offline tests cover reminder timing, persistence, rule inheritance, cancellation, deduplication, and sentence-boundary speech arbitration.

## Implementation validation (2026-10-13)

- Rust suite: 303 passed, 54 ignored; the new interactive reminder fixture was exercised separately. Hermes suite: 79 passed, 2 optional tests skipped.
- Formatting, `cargo check`, strict Clippy across all targets/features, and the release build passed. Dependency license notices were regenerated.
- Live winit/Skia MCP checks used 85 synthetic events: pagination, selecting an occurrence, saving an override, and dismissing a popup. Screenshots were inspected at normal and minimum supported sizes with 100%, 125%, and 150% scale overrides on a 125% Windows display. These are scale overrides, not OS DPI-transition tests.
- Windows foreground checks confirmed that both the initial and recreated popup avoided focus theft, retained `WS_EX_NOACTIVATE`, and hid on dismissal.
- Timing/storage tests cover phase catch-up, post-meeting repetition, snooze expiry, dismissal across synchronization/restart, series isolation, duplicate calendar copies, cancellation, rescheduling, inheritance/partial rule changes, transaction rollback, future database-version protection, bounded candidate scans, and audio eligibility without database locking. Speech arbitration tests cover sentence-boundary priority and cancellation.
- Real Microsoft sign-in/synchronization and account-driven spoken delivery are deferred at the user's request until they supply their public application ID. No website publication or forced Echo restart occurred. This feature does not change transcription processing.
