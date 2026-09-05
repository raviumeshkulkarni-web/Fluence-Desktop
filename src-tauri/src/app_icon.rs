// Fluence Windows - Foreground app icon extraction (Windows only)
// Raycast-style: when the recording overlay appears, it shows the icon + name
// of the app that currently owns the foreground so users instantly know what
// they are dictating into.
//
// Isolation contract: this module is 100% additive. It introduces no shared
// state, never touches the audio/clipboard/hotkey hot paths, and fails closed
// (returns None) on every error path - the overlay simply hides the icon chip.

use std::mem::size_of;
use std::path::Path;

use base64::Engine;
use serde::Serialize;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HBRUSH, HGDIOBJ,
};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{
    SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, DrawIconEx, GetForegroundWindow, GetSystemMetrics, GetWindowThreadProcessId,
    DI_NORMAL, HICON, SM_CXICON, SM_CYICON,
};

/// Payload returned to the overlay frontend.
#[derive(Serialize)]
pub struct ForegroundAppInfo {
    pub name: String,
    pub icon_data_url: String,
}

/// Tauri command - resolve the app currently owning the foreground and return
/// its display name plus a base64 PNG data URL for its large icon.
///
/// Returns `None` whenever the foreground app cannot be identified (including
/// when it is Fluence itself, or an elevated / protected process).
#[tauri::command]
pub fn get_foreground_app_icon() -> Option<ForegroundAppInfo> {
    match resolve_foreground_app() {
        Ok(info) => Some(info),
        Err(reason) => {
            log::debug!("get_foreground_app_icon: {reason}");
            None
        }
    }
}

fn resolve_foreground_app() -> Result<ForegroundAppInfo, String> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return Err("no foreground window".into());
    }

    let mut pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if pid == 0 {
        return Err("foreground window has no owning process".into());
    }
    if pid == unsafe { GetCurrentProcessId() } {
        // Fluence's own window - never show our own icon.
        return Err("foreground window belongs to Fluence".into());
    }

    let exe_path = query_process_path(pid)?;
    let name = Path::new(&exe_path)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "App".into());

    let icon = load_exe_icon(&exe_path)?;
    let data_url = icon_to_data_url(icon)?;
    Ok(ForegroundAppInfo {
        name,
        icon_data_url: data_url,
    })
}

fn query_process_path(pid: u32) -> Result<String, String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .map_err(|e| format!("OpenProcess failed: {e}"))?;

        let mut buffer = vec![0u16; 32768];
        let mut len = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(handle);

        result.map_err(|e| format!("QueryFullProcessImageNameW failed: {e}"))?;
        buffer.truncate(len as usize);
        Ok(String::from_utf16_lossy(&buffer))
    }
}

fn load_exe_icon(exe_path: &str) -> Result<HICON, String> {
    unsafe {
        let wide: Vec<u16> = exe_path.encode_utf16().chain(std::iter::once(0)).collect();

        let mut info = SHFILEINFOW::default();
        let mut flags = SHGFI_ICON | SHGFI_LARGEICON;

        let mut result = SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(FILE_ATTRIBUTE_NORMAL.0),
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            flags,
        );

        if result == 0 || info.hIcon.is_invalid() {
            // Retry without touching the file system so we also handle
            // protected / elevated application paths.
            flags |= SHGFI_USEFILEATTRIBUTES;
            result = SHGetFileInfoW(
                PCWSTR(wide.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(FILE_ATTRIBUTE_NORMAL.0),
                Some(&mut info),
                size_of::<SHFILEINFOW>() as u32,
                flags,
            );
        }

        if result == 0 || info.hIcon.is_invalid() {
            return Err("SHGetFileInfoW returned no icon".into());
        }
        Ok(info.hIcon)
    }
}

/// Rasterise an HICON into a 32bpp DIB and return it as a base64 PNG data URL.
fn icon_to_data_url(icon: HICON) -> Result<String, String> {
    unsafe {
        let width = GetSystemMetrics(SM_CXICON) as i32;
        let height = GetSystemMetrics(SM_CYICON) as i32;
        if width <= 0 || height <= 0 {
            let _ = DestroyIcon(icon);
            return Err("invalid icon dimensions".into());
        }

        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            let _ = DestroyIcon(icon);
            return Err("GetDC failed".into());
        }

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib = match CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(hbmp) => hbmp,
            Err(e) => {
                let _ = ReleaseDC(None, screen_dc);
                let _ = DestroyIcon(icon);
                return Err(format!("CreateDIBSection failed: {e}"));
            }
        };

        let mem_dc = CreateCompatibleDC(screen_dc);
        if mem_dc.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(dib.0));
            let _ = ReleaseDC(None, screen_dc);
            let _ = DestroyIcon(icon);
            return Err("CreateCompatibleDC failed".into());
        }

        let old = SelectObject(mem_dc, HGDIOBJ(dib.0));

        let result = (|| -> Result<String, String> {
            DrawIconEx(
                mem_dc,
                0,
                0,
                icon,
                width,
                height,
                0,
                HBRUSH::default(),
                DI_NORMAL,
            )
            .map_err(|e| format!("DrawIconEx failed: {e}"))?;

            let pixel_count = (width * height) as usize;
            let src = std::slice::from_raw_parts(bits as *const u8, pixel_count * 4);
            let mut rgba = vec![0u8; pixel_count * 4];
            for (row, rgba_row) in src.chunks_exact(4).zip(rgba.chunks_exact_mut(4)) {
                // DIB rows are BGRA; image crate wants RGBA.
                rgba_row[0] = row[2];
                rgba_row[1] = row[1];
                rgba_row[2] = row[0];
                rgba_row[3] = row[3];
            }

            let img = image::RgbaImage::from_raw(width as u32, height as u32, rgba)
                .ok_or_else(|| "failed to build RGBA image".to_string())?;

            let mut png: Vec<u8> = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .map_err(|e| format!("PNG encode failed: {e}"))?;

            let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
            Ok(format!("data:image/png;base64,{encoded}"))
        })();

        let _ = SelectObject(mem_dc, old);
        let _ = DeleteObject(HGDIOBJ(dib.0));
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(None, screen_dc);
        let _ = DestroyIcon(icon);

        result
    }
}
