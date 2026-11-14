//! Windows string resources and bitmap output for temporary in-game text.

use std::{ffi::c_void, os::windows::ffi::OsStrExt, path::Path, ptr};
use windows_sys::Win32::{
    Foundation::{FreeLibrary, HMODULE, RECT},
    Graphics::Gdi::*,
    System::LibraryLoader::{
        LoadLibraryExW, LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE,
    },
    UI::WindowsAndMessaging::LoadStringW,
};

/// Mapped Windows resource-only library.
pub struct StringResources(HMODULE);

impl StringResources {
    /// Opens the library for reading resources without executing its code.
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut path: Vec<_> = path.as_os_str().encode_wide().collect();
        if path.contains(&0) {
            return Err("resource library path contains NUL".into());
        }
        path.push(0);
        // SAFETY: The path is NUL-terminated and the flags map data only.
        let handle = unsafe {
            LoadLibraryExW(
                path.as_ptr(),
                0,
                LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE,
            )
        };
        if handle == 0 {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(Self(handle))
        }
    }

    /// Reads a string from the resource table.
    pub fn get(&self, id: u32) -> Option<String> {
        // A zero buffer size returns a borrowed pointer plus its UTF-16 length.
        let mut entry: *const u16 = ptr::null();
        // SAFETY: LoadStringW stores the pointer in `entry`. The mapping
        // remains alive until after the returned UTF-16 units are copied.
        unsafe {
            let len = LoadStringW(self.0, id, (&mut entry as *mut *const u16).cast(), 0);
            (len > 0 && !entry.is_null())
                .then(|| String::from_utf16_lossy(std::slice::from_raw_parts(entry, len as usize)))
        }
    }
}

impl Drop for StringResources {
    fn drop(&mut self) {
        // SAFETY: This type uniquely owns the mapping.
        unsafe { FreeLibrary(self.0) };
    }
}

/// Draws word-wrapped text with the system font on a translucent backing.
/// The returned height is the occupied portion of the fixed image.
pub fn caption(text: &str, width: u16, height: u16) -> Result<(u32, Vec<u8>), String> {
    const PADDING: i32 = 12;
    let (width, height) = (i32::from(width), i32::from(height));
    if width <= PADDING * 2 || height <= PADDING * 2 {
        return Err("caption image is too small".into());
    }
    let count = (width as usize)
        .checked_mul(height as usize)
        .and_then(|area| area.checked_mul(4))
        .filter(|&size| size <= isize::MAX as usize)
        .ok_or_else(|| "caption image is too large".to_owned())?;
    let text: Vec<_> = text.encode_utf16().collect();
    let len = i32::try_from(text.len()).map_err(|_| "caption exceeds GDI text length")?;
    let flags = DT_WORDBREAK | DT_NOPREFIX;
    let mut bounds = RECT {
        left: PADDING,
        top: PADDING,
        right: width - PADDING,
        bottom: height - PADDING,
    };
    // SAFETY: We own the memory DC and bitmap. GDI writes only within the
    // described DIB. The pixel slice is copied before either handle is freed.
    unsafe {
        let dc = CreateCompatibleDC(0);
        if dc == 0 {
            return Err("create caption DC".into());
        }
        SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
        let mut measured = bounds;
        let text_height = DrawTextW(dc, text.as_ptr(), len, &mut measured, flags | DT_CALCRECT);
        let occupied = text_height
            .saturating_add(PADDING * 2)
            .clamp(PADDING * 2, height);
        let bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            },
            bmiColors: [std::mem::zeroed()],
        };
        let mut pixels: *mut c_void = ptr::null_mut();
        let bitmap = CreateDIBSection(dc, &bitmap_info, DIB_RGB_COLORS, &mut pixels, 0, 0);
        if bitmap == 0 || pixels.is_null() {
            if bitmap != 0 {
                DeleteObject(bitmap);
            }
            DeleteDC(dc);
            return Err("create caption bitmap".into());
        }
        let previous = SelectObject(dc, bitmap);
        if previous == 0 || previous == -1 {
            DeleteObject(bitmap);
            DeleteDC(dc);
            return Err("select caption bitmap".into());
        }
        ptr::write_bytes(pixels.cast::<u8>(), 0, count);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, 0x00ff_ffff);
        bounds.bottom = occupied - PADDING;
        DrawTextW(dc, text.as_ptr(), len, &mut bounds, flags);
        GdiFlush();
        let rgba = std::slice::from_raw_parts(pixels.cast::<u8>(), count)
            .chunks_exact(4)
            .flat_map(|pixel| {
                [
                    pixel[2],
                    pixel[1],
                    pixel[0],
                    192.max(pixel[0]).max(pixel[1]).max(pixel[2]),
                ]
            })
            .collect();
        SelectObject(dc, previous);
        DeleteObject(bitmap);
        DeleteDC(dc);
        Ok((occupied as u32, rgba))
    }
}
