//! SendInput's Unicode keyboard path, for an explicit X "type" action.
//! No shell or clipboard fallback. No Enter, shortcuts, or arbitrary keycodes.

use std::ffi::c_void;

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

pub(crate) fn type_text(text: &str) -> Result<String, String> {
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
        if unsafe { GetForegroundWindow() } != original {
            return Err("Focus changed; typing was stopped".into());
        }
        let inputs = [stroke(unit, false), stroke(unit, true)];
        // SAFETY: a fixed two-event Unicode keyboard buffer is sent directly
        // to the foreground app. UIPI may refuse injected input.
        let sent = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<Input>() as i32,
            )
        };
        if sent != inputs.len() as u32 {
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
