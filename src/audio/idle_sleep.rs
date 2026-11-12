//! Keeps the computer from idle sleep while Hex records: an IOKit
//! no-idle-sleep assertion on macOS, a logind `idle:sleep` (else `idle`)
//! inhibitor on Linux, and a system-required power request on Windows. Each is held
//! until its guard drops. Starting one may wait on the OS, so callers start
//! it off the capture loop.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Guards alive now, whether or not the OS granted them.
static HELD: AtomicUsize = AtomicUsize::new(0);

/// Prevents idle sleep until dropped. When the OS refuses (logged), the
/// guard still counts as Hex recording ([`held`]).
pub fn prevent_sleep() -> Option<PreventSleep> {
    let platform = match platform::PreventSleep::start() {
        Ok(prevention) => Some(prevention),
        Err(error) => {
            tracing::warn!(%error, "could not prevent idle system sleep");
            None
        }
    };
    HELD.fetch_add(1, Ordering::AcqRel);
    Some(PreventSleep {
        _platform: platform,
    })
}

/// Whether Hex records now (a dictation past its hold, or a meeting): some
/// guard is alive.
pub fn held() -> bool {
    HELD.load(Ordering::Acquire) > 0
}

/// Idle-sleep prevention, held until dropped.
pub struct PreventSleep {
    _platform: Option<platform::PreventSleep>,
}

impl Drop for PreventSleep {
    fn drop(&mut self) {
        HELD.fetch_sub(1, Ordering::AcqRel);
    }
}

/// An optional, explicitly requested, time-limited system idle inhibitor.
/// Unlike recording prevention, it does NOT increment HELD: using this utility
/// cannot suppress speech-model idle unloading or pretend a dictation exists.
struct WakeSession {
    until: Option<std::time::Instant>,
    inhibition: Option<platform::PreventSleep>,
}

struct WakeShared {
    state: std::sync::Mutex<WakeSession>,
    changed: std::sync::Condvar,
}

static KEEP_AWAKE: std::sync::OnceLock<Result<std::sync::Arc<WakeShared>, String>> =
    std::sync::OnceLock::new();

fn awake_shared() -> Result<std::sync::Arc<WakeShared>, String> {
    KEEP_AWAKE
        .get_or_init(|| {
            let shared = std::sync::Arc::new(WakeShared {
                state: std::sync::Mutex::new(WakeSession {
                    until: None,
                    inhibition: None,
                }),
                changed: std::sync::Condvar::new(),
            });
            let worker = shared.clone();
            std::thread::Builder::new()
                .name("x-keep-awake-timer".into())
                .spawn(move || {
                    loop {
                        let mut state = worker.state.lock().unwrap_or_else(|e| e.into_inner());
                        match state.until {
                            None => {
                                state = worker.changed.wait(state).unwrap_or_else(|e| e.into_inner());
                                drop(state);
                            }
                            Some(until) if std::time::Instant::now() >= until => {
                                state.until = None;
                                state.inhibition.take();
                                drop(state);
                            }
                            Some(until) => {
                                let timeout = until.saturating_duration_since(std::time::Instant::now());
                                let (state, _) = worker
                                    .changed
                                    .wait_timeout(state, timeout)
                                    .unwrap_or_else(|e| e.into_inner());
                                drop(state);
                            }
                        }
                    }
                })
                .map_err(|e| format!("Could not start Keep Awake timer: {e}"))?;
            Ok(shared)
        })
        .clone()
}

fn checked_minutes(minutes: u32) -> Result<std::time::Duration, String> {
    (1..=480)
        .contains(&minutes)
        .then_some(std::time::Duration::from_secs(u64::from(minutes) * 60))
        .ok_or_else(|| "Keep Awake duration must be 1–480 minutes".to_string())
}

/// Hold the OS's idle inhibitor for at most eight hours. Consecutive starts
/// replace the old duration and guard rather than leaking timer threads.
pub fn keep_awake_start(minutes: u32) -> Result<(), String> {
    let duration = checked_minutes(minutes)?;
    let shared = awake_shared()?;
    let inhibitor = platform::PreventSleep::start()
        .map_err(|e| format!("This desktop could not inhibit idle sleep: {e}"))?;
    let mut session = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    session.until = Some(std::time::Instant::now() + duration);
    session.inhibition = Some(inhibitor);
    shared.changed.notify_one();
    Ok(())
}

/// Release the inhibitor immediately; never terminate a running recording.
pub fn keep_awake_stop() -> bool {
    let Some(Ok(shared)) = KEEP_AWAKE.get() else {
        return false;
    };
    let mut session = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    let was_active = session.until.take().is_some();
    session.inhibition.take();
    shared.changed.notify_one();
    was_active
}

pub fn keep_awake_remaining() -> Option<std::time::Duration> {
    let Some(Ok(shared)) = KEEP_AWAKE.get() else {
        return None;
    };
    let session = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    session
        .until
        .filter(|_| session.inhibition.is_some())
        .map(|until| until.saturating_duration_since(std::time::Instant::now()))
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;

    use objc2_core_foundation::CFString;

    const POWER_ASSERTION_LEVEL_ON: u32 = 255;

    pub struct PreventSleep {
        assertion_id: u32,
    }

    impl PreventSleep {
        pub(super) fn start() -> std::io::Result<Self> {
            let assertion_type = CFString::from_static_str("NoIdleSleepAssertion");
            let assertion_name = CFString::from_static_str("Hex intentional recording");
            let mut assertion_id = 0;
            // SAFETY: Both Core Foundation strings remain alive for the call and
            // assertion_id points to writable, correctly sized storage. IOKit
            // retains the assertion independently until IOPMAssertionRelease.
            let status = unsafe {
                IOPMAssertionCreateWithName(
                    (&*assertion_type as *const CFString).cast(),
                    POWER_ASSERTION_LEVEL_ON,
                    (&*assertion_name as *const CFString).cast(),
                    &mut assertion_id,
                )
            };
            if status != 0 {
                return Err(std::io::Error::other(format!(
                    "IOPMAssertionCreateWithName failed with IOReturn 0x{:08x}",
                    status as u32
                )));
            }
            Ok(Self { assertion_id })
        }
    }

    impl Drop for PreventSleep {
        fn drop(&mut self) {
            // SAFETY: assertion_id was returned by a successful create call and
            // this guard is its sole owner, so release occurs exactly once.
            let status = unsafe { IOPMAssertionRelease(self.assertion_id) };
            if status != 0 {
                tracing::warn!(
                    status = format_args!("0x{:08x}", status as u32),
                    "could not release idle-sleep assertion"
                );
            }
        }
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: *const c_void,
            assertion_level: u32,
            assertion_name: *const c_void,
            assertion_id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(assertion_id: u32) -> i32;
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::time::Duration;

    use zbus::OwnedFd;

    const TIMEOUT: Duration = Duration::from_millis(500);

    /// logind keeps the inhibitor while this descriptor is open.
    pub struct PreventSleep {
        _inhibitor: OwnedFd,
    }

    impl PreventSleep {
        /// Blocks idle and sleep, or only idle where polkit refuses to let
        /// a process outside the active seat session, such as Hex's user
        /// service, block sleep.
        pub(super) fn start() -> std::io::Result<Self> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async {
                let bus = tokio::time::timeout(TIMEOUT, zbus::Connection::system())
                    .await
                    .map_err(|_| std::io::Error::other("the system D-Bus did not answer"))?
                    .map_err(std::io::Error::other)?;
                match inhibit(&bus, "idle:sleep").await {
                    Ok(inhibitor) => Ok(inhibitor),
                    Err(error) => {
                        tracing::debug!(%error, "logind refused to block sleep; blocking idle only");
                        inhibit(&bus, "idle").await
                    }
                }
            })
        }
    }

    async fn inhibit(bus: &zbus::Connection, what: &str) -> std::io::Result<PreventSleep> {
        let body = (what, "Hex", "Recording", "block");
        let call = bus.call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "Inhibit",
            &body,
        );
        let reply = tokio::time::timeout(TIMEOUT, call)
            .await
            .map_err(|_| std::io::Error::other("logind did not answer"))?
            .map_err(std::io::Error::other)?;
        let inhibitor: OwnedFd = reply.body().deserialize().map_err(std::io::Error::other)?;
        Ok(PreventSleep {
            _inhibitor: inhibitor,
        })
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Power::{
        PowerClearRequest, PowerCreateRequest, PowerRequestSystemRequired, PowerSetRequest,
    };
    use windows_sys::Win32::System::Threading::{
        POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0,
    };

    const POWER_REQUEST_CONTEXT_VERSION: u32 = 0;

    /// A power request, unlike `SetThreadExecutionState`, is not tied to the
    /// thread that made it.
    pub struct PreventSleep {
        request: HANDLE,
    }

    // SAFETY: A power request handle may be used and closed from any thread.
    unsafe impl Send for PreventSleep {}

    impl PreventSleep {
        pub(super) fn start() -> std::io::Result<Self> {
            let mut reason: Vec<u16> = "Hex is recording".encode_utf16().chain([0]).collect();
            let context = REASON_CONTEXT {
                Version: POWER_REQUEST_CONTEXT_VERSION,
                Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
                Reason: REASON_CONTEXT_0 {
                    SimpleReasonString: reason.as_mut_ptr(),
                },
            };
            // SAFETY: `context` and its string outlive the call, which copies them.
            let request = unsafe { PowerCreateRequest(&context) };
            if request.is_null() || request == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: `request` is the valid handle just created.
            if unsafe { PowerSetRequest(request, PowerRequestSystemRequired) } == 0 {
                let error = std::io::Error::last_os_error();
                // SAFETY: The handle is ours and closed once.
                unsafe { CloseHandle(request) };
                return Err(error);
            }
            Ok(Self { request })
        }
    }

    impl Drop for PreventSleep {
        fn drop(&mut self) {
            // SAFETY: The request was set in `start`, and this guard closes it once.
            unsafe {
                PowerClearRequest(self.request, PowerRequestSystemRequired);
                CloseHandle(self.request);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "exercises the OS's native idle-sleep prevention"]
    fn native_idle_sleep_prevention_acquires_and_releases() {
        drop(super::platform::PreventSleep::start().unwrap());
    }
}
