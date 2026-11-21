//! SendInput's Unicode keyboard path, for an explicit X "type" action.
//! No shell or clipboard fallback. No Enter, shortcuts, or arbitrary keycodes.

use std::ffi::c_void;

use crate::cancellation::Cancellation;

const INPUT_KEYBOARD: u32 = 1;
const KEYEVENTF_KEYUP: u32 = 0x0002;
const KEYEVENTF_UNICODE: u32 = 0x0004;

#[repr(C)]
#[derive(Clone, Copy)]
struct KeybdInput {
    vk: u16,
    scan: u16,
    flags: u32,
    time: u32,
    extra: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
union InputData {
    keyboard: KeybdInput,
    // 32-byte/aligned union on supported Windows 11 x64 and ARM64 targets.
    reserved: [usize; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Input {
    kind: u32,
    data: InputData,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn SendInput(count: u32, data: *const Input, size: i32) -> u32;
    fn GetForegroundWindow() -> *mut c_void;
}

fn virtual_key(key: u16, up: bool) -> Input {
    Input {
        kind: INPUT_KEYBOARD,
        data: InputData {
            keyboard: KeybdInput {
                vk: key,
                scan: 0,
                flags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                extra: 0,
            },
        },
    }
}

fn send_input(events: &[Input]) -> usize {
    // SAFETY: these inputs are initialized keyboard events for a bounded batch.
    (unsafe {
        SendInput(
            events.len() as u32,
            events.as_ptr(),
            std::mem::size_of::<Input>() as i32,
        )
    }) as usize
}

fn send_complete(events: &[Input], send: impl Fn(&[Input]) -> usize) -> bool {
    let sent = send(events).min(events.len());
    if sent == events.len() {
        return true;
    }
    let mut held: Vec<KeybdInput> = Vec::new();
    for input in &events[..sent] {
        // SAFETY: all inputs here were built by virtual_key or stroke.
        let key = unsafe { input.data.keyboard };
        if key.flags & KEYEVENTF_KEYUP != 0 {
            held.retain(|down| down.vk != key.vk || down.scan != key.scan);
        } else {
            held.push(key);
        }
    }
    for mut key in held.into_iter().rev() {
        key.flags |= KEYEVENTF_KEYUP;
        send(&[Input {
            kind: INPUT_KEYBOARD,
            data: InputData { keyboard: key },
        }]);
    }
    false
}

pub(crate) fn browser_hotkey(
    op: crate::browser_tab::Op,
    expected_hwnd: usize,
) -> Result<(), String> {
    const CTRL: u16 = 0x11;
    const SHIFT: u16 = 0x10;
    const T: u16 = 0x54;
    const W: u16 = 0x57;

    // The caller validated the foreground window against an installed
    // browser and re-observed its exact identity. Recheck immediately before
    // input is injected; no arbitrary key strings can reach this function.
    if (unsafe { GetForegroundWindow() } as usize) != expected_hwnd || expected_hwnd == 0 {
        return Err("Focus changed before the browser shortcut".into());
    }
    let events = match op {
        crate::browser_tab::Op::New => vec![
            virtual_key(CTRL, false),
            virtual_key(T, false),
            virtual_key(T, true),
            virtual_key(CTRL, true),
        ],
        crate::browser_tab::Op::Close => vec![
            virtual_key(CTRL, false),
            virtual_key(W, false),
            virtual_key(W, true),
            virtual_key(CTRL, true),
        ],
        crate::browser_tab::Op::Reopen => vec![
            virtual_key(CTRL, false),
            virtual_key(SHIFT, false),
            virtual_key(T, false),
            virtual_key(T, true),
            virtual_key(SHIFT, true),
            virtual_key(CTRL, true),
        ],
    };
    if !send_complete(&events, send_input) {
        return Err("Windows did not accept the complete browser shortcut".into());
    }
    Ok(())
}

fn stroke(unit: u16, key_up: bool) -> Input {
    Input {
        kind: INPUT_KEYBOARD,
        data: InputData {
            keyboard: KeybdInput {
                vk: 0,
                scan: unit,
                flags: KEYEVENTF_UNICODE | if key_up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                extra: 0,
            },
        },
    }
}

pub(crate) fn type_text(text: &str, stop: &Cancellation) -> Result<String, String> {
    if text.trim().is_empty() || text.chars().count() > 300 {
        return Err("Typed text must contain 1-300 characters".into());
    }
    if text.chars().any(char::is_control) {
        return Err("Typed text must not contain control characters".into());
    }
    // SAFETY: the opaque HWND is only compared for identity, never dereferenced.
    let original = unsafe { GetForegroundWindow() };
    if original.is_null() {
        return Err("No Windows window has focus".into());
    }
    for unit in text.encode_utf16() {
        stop.check()?;
        if unsafe { GetForegroundWindow() } != original {
            return Err("Focus changed; typing was stopped".into());
        }
        let inputs = [stroke(unit, false), stroke(unit, true)];
        if !send_complete(&inputs, send_input) {
            return Err("Windows refused Unicode typing (possibly an elevated app)".into());
        }
    }
    Ok(format!(
        "Sent {} characters to the focused window (application acceptance is not verified)",
        text.chars().count()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_send_releases_only_keys_the_batch_held() {
        let events = [
            virtual_key(0x11, false),
            virtual_key(0x54, false),
            virtual_key(0x54, true),
            virtual_key(0x11, true),
        ];
        for (accepted, expected) in [
            (0, vec![]),
            (1, vec![0x11]),
            (2, vec![0x54, 0x11]),
            (3, vec![0x11]),
        ] {
            let calls = std::cell::Cell::new(0);
            let released = std::cell::RefCell::new(Vec::new());
            assert!(!send_complete(&events, |inputs| {
                let i = calls.get();
                calls.set(i + 1);
                if i == 0 {
                    return accepted;
                }
                // SAFETY: inputs were constructed as keyboard events.
                let key = unsafe { inputs[0].data.keyboard };
                assert_ne!(key.flags & KEYEVENTF_KEYUP, 0);
                released.borrow_mut().push(key.vk);
                1
            }));
            assert_eq!(*released.borrow(), expected);
        }
    }

    #[test]
    fn sendinput_layout_matches_win11_input_abi() {
        assert_eq!(std::mem::size_of::<KeybdInput>(), 24);
        assert_eq!(std::mem::size_of::<Input>(), 40);
        let down = stroke(0x0061, false);
        let up = stroke(0x0061, true);
        // SAFETY: read the initialized keyboard union arm.
        unsafe {
            assert_eq!(down.data.keyboard.scan, 0x0061);
            assert_eq!(down.data.keyboard.flags, KEYEVENTF_UNICODE);
            assert_eq!(up.data.keyboard.flags, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
        }
    }
}
