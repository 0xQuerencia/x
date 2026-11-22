//! User-requested, bounded Win32 screen capture. No background polling,
//! disk cache, screenshot uploads, clipboard or shell subprocesses.

use std::ffi::c_void;
use std::ptr;

const SRCCOPY: u32 = 0x00CC0020;
const BI_RGB: u32 = 0;
const DIB_RGB_COLORS: u32 = 0;
const SM_XVIRTUALSCREEN: i32 = 76;
const SM_YVIRTUALSCREEN: i32 = 77;
const SM_CXVIRTUALSCREEN: i32 = 78;
const SM_CYVIRTUALSCREEN: i32 = 79;
const MAX_BMP_BYTES: usize = 64 * 1024 * 1024;

#[repr(C)]
struct BitmapInfoHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_pixels_per_meter: i32,
    y_pixels_per_meter: i32,
    colors_used: u32,
    colors_important: u32,
}

#[repr(C)]
struct BitmapInfo {
    header: BitmapInfoHeader,
    colors: [u32; 1],
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetDC(window: *mut c_void) -> *mut c_void;
    fn ReleaseDC(window: *mut c_void, dc: *mut c_void) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateCompatibleDC(dc: *mut c_void) -> *mut c_void;
    fn CreateCompatibleBitmap(dc: *mut c_void, width: i32, height: i32) -> *mut c_void;
    fn SelectObject(dc: *mut c_void, object: *mut c_void) -> *mut c_void;
    fn BitBlt(
        destination: *mut c_void,
        dx: i32,
        dy: i32,
        width: i32,
        height: i32,
        source: *mut c_void,
        sx: i32,
        sy: i32,
        operation: u32,
    ) -> i32;
    fn GetDIBits(
        dc: *mut c_void,
        bitmap: *mut c_void,
        start: u32,
        scan_lines: u32,
        pixels: *mut c_void,
        info: *mut BitmapInfo,
        usage: u32,
    ) -> i32;
    fn DeleteDC(dc: *mut c_void) -> i32;
    fn DeleteObject(object: *mut c_void) -> i32;
}

/// Own all native GDI handles on error paths, including partial allocations.
struct Handles {
    screen: *mut c_void,
    memory: *mut c_void,
    bitmap: *mut c_void,
    old_bitmap: *mut c_void,
}

impl Drop for Handles {
    fn drop(&mut self) {
        // SAFETY: all handles were returned by Win32 APIs in this call. The
        // previous selected bitmap is restored before the owned one is freed.
        unsafe {
            if !self.memory.is_null() && !self.old_bitmap.is_null() {
                SelectObject(self.memory, self.old_bitmap);
            }
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
            if !self.memory.is_null() {
                DeleteDC(self.memory);
            }
            if !self.screen.is_null() {
                ReleaseDC(ptr::null_mut(), self.screen);
            }
        }
    }
}

fn dimensions(width: i32, height: i32) -> Result<usize, String> {
    if width <= 0 || height <= 0 {
        return Err("Windows returned invalid screen geometry".into());
    }
    let byte_count = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or("Screen capture dimensions overflow")?;
    if byte_count > MAX_BMP_BYTES.saturating_sub(54) {
        return Err("Windows screen capture exceeds the 64 MiB safety limit".into());
    }
    Ok(byte_count)
}

fn encode_bmp(width: i32, height: i32, pixels: &[u8]) -> Result<Vec<u8>, String> {
    let image_bytes = dimensions(width, height)?;
    if pixels.len() != image_bytes {
        return Err("Windows returned an incomplete screen capture".into());
    }
    let total = 54usize
        .checked_add(image_bytes)
        .ok_or("BMP size overflow")?;
    let total_u32 = u32::try_from(total).map_err(|_| "BMP size overflow")?;
    let image_u32 = u32::try_from(image_bytes).map_err(|_| "BMP size overflow")?;

    let mut bmp = Vec::with_capacity(total);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&total_u32.to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&width.to_le_bytes());
    bmp.extend_from_slice(&(-height).to_le_bytes()); // Top-down BGRA scanlines.
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&32u16.to_le_bytes());
    bmp.extend_from_slice(&BI_RGB.to_le_bytes());
    bmp.extend_from_slice(&image_u32.to_le_bytes());
    bmp.extend_from_slice(&[0; 16]);
    bmp.extend_from_slice(pixels);
    Ok(bmp)
}

/// Capture the whole virtual desktop only when the local screenshot action
/// has already passed normal X permission and intent checks. Windows secure
/// desktops may be black or refused; we never attempt to bypass protection.
pub(super) fn capture_screen_bmp() -> Result<Vec<u8>, String> {
    // SAFETY: GetSystemMetrics is read-only and accepts these fixed constants.
    let (x, y, width, height) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let size = dimensions(width, height)?;
    // SAFETY: null HWND means the virtual desktop device context.
    let mut handles = Handles {
        screen: unsafe { GetDC(ptr::null_mut()) },
        memory: ptr::null_mut(),
        bitmap: ptr::null_mut(),
        old_bitmap: ptr::null_mut(),
    };
    if handles.screen.is_null() {
        return Err("Windows screen capture could not access the desktop".into());
    }
    // SAFETY: these calls use valid HDCs and bounded, checked dimensions.
    handles.memory = unsafe { CreateCompatibleDC(handles.screen) };
    if handles.memory.is_null() {
        return Err("Windows could not create a screenshot device context".into());
    }
    handles.bitmap = unsafe { CreateCompatibleBitmap(handles.screen, width, height) };
    if handles.bitmap.is_null() {
        return Err("Windows could not allocate a screenshot bitmap".into());
    }
    handles.old_bitmap = unsafe { SelectObject(handles.memory, handles.bitmap) };
    if handles.old_bitmap.is_null() {
        return Err("Windows could not select a screenshot bitmap".into());
    }
    let copied = unsafe {
        BitBlt(
            handles.memory,
            0,
            0,
            width,
            height,
            handles.screen,
            x,
            y,
            SRCCOPY,
        )
    };
    if copied == 0 {
        return Err("Windows refused the screen capture".into());
    }
    // GetDIBits requires the bitmap not be selected into a device context.
    let restored = unsafe { SelectObject(handles.memory, handles.old_bitmap) };
    if restored != handles.bitmap {
        return Err("Windows could not restore the screenshot device context".into());
    }
    handles.old_bitmap = ptr::null_mut();

    let mut info = BitmapInfo {
        header: BitmapInfoHeader {
            size: 40,
            width,
            height: -height,
            planes: 1,
            bit_count: 32,
            compression: BI_RGB,
            size_image: size as u32,
            x_pixels_per_meter: 0,
            y_pixels_per_meter: 0,
            colors_used: 0,
            colors_important: 0,
        },
        colors: [0],
    };
    let mut pixels = vec![0; size];
    let copied_lines = unsafe {
        GetDIBits(
            handles.screen,
            handles.bitmap,
            0,
            height as u32,
            pixels.as_mut_ptr().cast(),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    if copied_lines != height {
        return Err("Windows returned an incomplete screenshot".into());
    }
    encode_bmp(width, height, &pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bmp_header_describes_top_down_32_bit_pixels() {
        let bmp = encode_bmp(2, 1, &[0, 0, 255, 0, 1, 2, 3, 0]).unwrap();
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[2..6].try_into().unwrap()), 62);
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 54);
        assert_eq!(i32::from_le_bytes(bmp[18..22].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(bmp[22..26].try_into().unwrap()), -1);
        assert_eq!(u16::from_le_bytes(bmp[28..30].try_into().unwrap()), 32);
        assert_eq!(&bmp[54..], &[0, 0, 255, 0, 1, 2, 3, 0]);
    }

    #[test]
    fn invalid_geometry_and_short_captures_fail_closed() {
        assert!(dimensions(0, 1).is_err());
        assert!(dimensions(1, -1).is_err());
        assert!(dimensions(i32::MAX, i32::MAX).is_err());
        assert!(encode_bmp(2, 2, &[0; 15]).is_err());
        assert!(encode_bmp(2, 2, &[0; 17]).is_err());
    }
}
