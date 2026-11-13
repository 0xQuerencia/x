//! Explicit, short-lived clipboard-change capture for X Quick on Linux/Wayland.
//! Disabled by default. Stops on timeout, user request or process exit.
//! No network, no screen access and no capture of image/file MIME types.
#[cfg(target_os = "linux")]
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
#[cfg(target_os = "linux")]
use std::time::Duration;
use std::time::Instant;

#[cfg(target_os = "linux")]
use crate::desktop::capture;
#[cfg(target_os = "linux")]
use crate::quick::clipboard_history::Store;
#[cfg(target_os = "linux")]
use crate::tools::which;

#[cfg(target_os = "linux")]
const CAPTURE_INTERVAL: Duration = Duration::from_secs(1);
#[cfg(target_os = "linux")]
const MAX_TEXT: usize = 16 * 1024;
#[cfg(target_os = "linux")]
const MAX_MIME: usize = 4096;
const MAX_MINUTES: u32 = 30;

#[derive(Default)]
struct Session {
    generation: u64,
    expires: Option<Instant>,
}

type Shared = Arc<(Mutex<Session>, Condvar)>;
static ACTIVE: OnceLock<Shared> = OnceLock::new();

#[cfg(target_os = "linux")]
fn shared() -> &'static Shared {
    ACTIVE.get_or_init(|| Arc::new((Mutex::new(Session::default()), Condvar::new())))
}

fn locked<'a>(mutex: &'a Mutex<Session>) -> std::sync::MutexGuard<'a, Session> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An offer marked concealed by a supported password manager is never read.
#[cfg(target_os = "linux")]
fn confidential_offer(mimes: &str) -> bool {
    let lower = mimes.to_ascii_lowercase();
    [
        "passwordmanagerhint",
        "keepass",
        "secret",
        "sensitive",
        "x-kde-password",
        "x-gnome-password",
    ]
    .iter()
    .any(|pattern| lower.contains(pattern))
}

/// Best-effort text redaction. Not a substitute for an app-specific privacy
/// protocol: unmarked secrets can still be copied and must be erased by the user.
#[cfg(target_os = "linux")]
fn likely_secret(text: &str) -> bool {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    if trimmed.is_empty() || trimmed.len() > MAX_TEXT || trimmed.contains('\0') {
        return true;
    }
    if [
        "-----begin ",
        "ghp_",
        "gho_",
        "github_pat_",
        "sk-",
        "api_key=",
        "password=",
        "authorization: bearer",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return true;
    }
    // A long single token is much more likely to be a password/API key than
    // ordinary prose. Links remain useful and are exempt.
    trimmed.len() >= 24
        && !trimmed.chars().any(char::is_whitespace)
        && !lower.starts_with("https://")
        && !lower.starts_with("http://")
        && !lower.starts_with("www.")
}

#[cfg(target_os = "linux")]
fn current_text() -> Option<String> {
    let executable = which("wl-paste")?;
    let (ok, types) = capture(
        Command::new(&executable).arg("--list-types"),
        Duration::from_secs(2),
    )
    .ok()?;
    if !ok || types.len() > MAX_MIME {
        return None;
    }
    let types = String::from_utf8(types).ok()?;
    if confidential_offer(&types) || !types.to_ascii_lowercase().contains("text/plain") {
        return None;
    }
    let (ok, data) = capture(
        Command::new(&executable).args(["--no-newline", "--type", "text/plain"]),
        Duration::from_secs(2),
    )
    .ok()?;
    if !ok || data.len() > MAX_TEXT {
        return None;
    }
    let text = String::from_utf8(data).ok()?;
    (!likely_secret(&text)).then_some(text)
}

#[cfg(target_os = "linux")]
fn run(shared: Shared, generation: u64) {
    let (mutex, cv) = &*shared;
    let mut last = String::new();
    loop {
        {
            let state = locked(mutex);
            if state.generation != generation || state.expires.is_none_or(|at| Instant::now() >= at)
            {
                break;
            }
        }
        if let Some(text) = current_text()
            && last != text
        {
            // Hold the state lock while persisting: once Stop returns,
            // no later capture from this worker can hit disk.
            let state = locked(mutex);
            if state.generation != generation || state.expires.is_none_or(|at| Instant::now() >= at)
            {
                break;
            }
            if let Ok(store) = Store::user()
                && store.save(&text).is_ok()
            {
                last = text;
            }
        }
        let guard = locked(mutex);
        let _wait = cv.wait_timeout(guard, CAPTURE_INTERVAL);
    }
    let mut state = locked(mutex);
    if state.generation == generation {
        state.expires = None;
    }
}

/// Start an explicit, time-bounded capture session. No persistent opt-in:
/// restarting X never resumes it. The user chooses when to start again.
pub fn start(minutes: u32) -> Result<String, String> {
    if !(1..=MAX_MINUTES).contains(&minutes) {
        return Err("Clipboard capture sessions must last 1–30 minutes".into());
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("Clipboard-change capture is currently available only on Linux Wayland".into())
    }
    #[cfg(target_os = "linux")]
    {
        which("wl-paste").ok_or("Install wl-clipboard and use a Wayland session")?;
        let shared = shared().clone();
        let (mutex, cv) = &*shared;
        let mut state = locked(mutex);
        state.generation = state.generation.wrapping_add(1);
        state.expires = Some(Instant::now() + Duration::from_secs(u64::from(minutes) * 60));
        let generation = state.generation;
        cv.notify_all();
        drop(state);
        std::thread::Builder::new()
            .name("x-clipboard-session".into())
            .spawn(move || run(shared, generation))
            .map_err(|error| {
                stop();
                format!("Could not start clipboard session: {error}")
            })?;
        Ok(format!(
            "Text clipboard capture enabled for {minutes} minute(s). Only opted-in session, local 24-hour retention. Avoid copying secrets."
        ))
    }
}

/// Stop new reads and wait for any in-flight store operation to release its lock.
pub fn stop() -> bool {
    let Some(shared) = ACTIVE.get() else {
        return false;
    };
    let (mutex, cv) = &**shared;
    let mut state = locked(mutex);
    let was_active = state.expires.take().is_some();
    state.generation = state.generation.wrapping_add(1);
    cv.notify_all();
    was_active
}

pub fn active() -> bool {
    let Some(shared) = ACTIVE.get() else {
        return false;
    };
    let state = locked(&shared.0);
    state.expires.is_some_and(|at| Instant::now() < at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn recognizes_secret_markers_and_long_tokens() {
        assert!(confidential_offer(
            "text/plain\napplication/x-kde-passwordManagerHint"
        ));
        assert!(confidential_offer("application/x-keepassxc"));
        assert!(!confidential_offer("text/plain;charset=utf-8"));
        assert!(likely_secret("sk-1234567890123456789"));
        assert!(likely_secret("1234567890abcdefghijABCDEFGHIJ"));
        assert!(!likely_secret("https://example.com/some-long-link"));
        assert!(!likely_secret(
            "A useful sentence with multiple ordinary words."
        ));
    }

    #[test]
    fn is_off_by_default_and_rejects_out_of_range() {
        assert!(!active());
        assert!(!stop());
        assert!(start(0).is_err());
        assert!(start(31).is_err());
    }
}
