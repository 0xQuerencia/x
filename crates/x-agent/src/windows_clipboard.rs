//! Explicit, owner-directed Unicode clipboard writes on Windows.
//! Never reads prior clipboard content and never invokes a shell/PowerShell.

use std::ffi::c_void;
use std::ptr;
use std::time::Duration;

use crate::clipboard::{MAX_COPY, validate};

type Handle = *mut c_void;
const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 0x0002;

#[link(name = "user32")]
unsafe extern "system" {
    fn OpenClipboard(owner: *mut c_void) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn SetClipboardData(format: u32, data: Handle) -> Handle;
    fn GetClipboardData(format: u32) -> Handle;
    fn CreateWindowExW(
        extended_style: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: Handle,
        menu: Handle,
        instance: Handle,
        param: Handle,
    ) -> Handle;
    fn DestroyWindow(window: Handle) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> Handle;
    fn GlobalFree(memory: Handle) -> Handle;
    fn GlobalLock(memory: Handle) -> *mut c_void;
    fn GlobalUnlock(memory: Handle) -> i32;
    fn GlobalSize(memory: Handle) -> usize;
}

struct ClipboardGuard {
    owner: Handle,
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: close the clipboard before destroying our hidden owner
        // window; a null owner would make SetClipboardData fail after EmptyClipboard.
        unsafe {
            CloseClipboard();
            DestroyWindow(self.owner);
        };
    }
}

fn open_clipboard() -> Result<ClipboardGuard, String> {
    let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
    // SAFETY: STATIC is a registered Windows class. A style=0 window is
    // invisible, belongs to this process, and owns this clipboard transaction.
    let owner = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            ptr::null(),
            0,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if owner.is_null() {
        return Err("Cannot create private Windows clipboard owner".into());
    }
    for _ in 0..10 {
        if unsafe { OpenClipboard(owner) } != 0 {
            return Ok(ClipboardGuard { owner });
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    unsafe { DestroyWindow(owner) };
    Err("Windows clipboard is busy".into())
}

pub(crate) fn copy(text: &str) -> Result<String, String> {
    validate(text)?;
    let encoded: Vec<u16> = text.encode_utf16().chain([0]).collect();
    if encoded.len() > 2 * MAX_COPY + 1 {
        return Err("Clipboard text is too large".into());
    }
    let bytes = encoded.len() * std::mem::size_of::<u16>();
    let _guard = open_clipboard()?;

    // SAFETY: allocate movable global memory as required by CF_UNICODETEXT;
    // once SetClipboardData succeeds, the operating system owns the buffer.
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if memory.is_null() {
            return Err("Cannot allocate Windows clipboard memory".into());
        }
        let locked = GlobalLock(memory).cast::<u16>();
        if locked.is_null() {
            GlobalFree(memory);
            return Err("Cannot access Windows clipboard memory".into());
        }
        ptr::copy_nonoverlapping(encoded.as_ptr(), locked, encoded.len());
        GlobalUnlock(memory);

        if EmptyClipboard() == 0 {
            GlobalFree(memory);
            return Err("Windows refused to clear clipboard for this copy".into());
        }
        if SetClipboardData(CF_UNICODETEXT, memory).is_null() {
            GlobalFree(memory);
            return Err("Windows refused clipboard text".into());
        }

        // Verify only the value just written; never inspect the previous owner
        // or retain unrelated clipboard content.
        let current = GetClipboardData(CF_UNICODETEXT);
        if current.is_null() || GlobalSize(current) < bytes {
            return Err("Windows clipboard did not retain the new text".into());
        }
        let read = GlobalLock(current).cast::<u16>();
        if read.is_null() {
            return Err("Cannot verify Windows clipboard text".into());
        }
        let matches = std::slice::from_raw_parts(read, encoded.len()) == encoded.as_slice();
        GlobalUnlock(current);
        if !matches {
            return Err("Clipboard changed during verification".into());
        }
    }
    Ok(format!("Copied {} characters", text.chars().count()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_is_bounded_before_touching_the_clipboard() {
        assert!(validate("hello").is_ok());
        assert!(validate("hello\0world").is_err());
        assert!(validate(&"x".repeat(MAX_COPY + 1)).is_err());
    }
}
