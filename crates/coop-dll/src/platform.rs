//! Windows, or Wine / Proton (Linux, Steam Deck)? Wine's ntdll exports `wine_get_version`,
//! Windows' does not (the usual check). Under Proton the game runs in its own prefix, so
//! `%LOCALAPPDATA%` is `steamapps/compatdata/814380/pfx/drive_c/users/steamuser/AppData/Local`
//! on the Linux side; Wine's `wine_get_unix_file_name` gives that path for the log.

use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn GetProcessHeap() -> *mut c_void;
    fn HeapFree(heap: *mut c_void, flags: u32, mem: *mut c_void) -> i32;
}

/// An export of an already loaded module (0 = no such module or export).
fn export(dll: &str, name: &CStr) -> usize {
    let w: Vec<u16> = dll.encode_utf16().chain(std::iter::once(0)).collect();
    let m = unsafe { GetModuleHandleW(w.as_ptr()) };
    if m.is_null() {
        return 0;
    }
    unsafe { GetProcAddress(m, name.as_ptr() as *const u8) as usize }
}

fn c_text(p: *const c_char) -> String {
    if p.is_null() {
        return "?".to_owned();
    }
    // SAFETY: Wine returns a NUL-terminated static string.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Wine's version ("9.0" etc.) when the game runs under Wine / Proton, None on Windows.
pub fn wine_version() -> Option<&'static str> {
    static V: OnceLock<Option<String>> = OnceLock::new();
    V.get_or_init(|| {
        let f = export("ntdll.dll", c"wine_get_version");
        // SAFETY: `const char *CDECL wine_get_version(void)`.
        (f != 0).then(|| c_text(unsafe { std::mem::transmute::<usize, unsafe extern "C" fn() -> *const c_char>(f)() }))
    })
    .as_deref()
}

pub fn is_wine() -> bool {
    wine_version().is_some()
}

/// "Linux 6.5.0" from Wine's `wine_get_host_version` (None on Windows or without the export).
fn host_os() -> Option<String> {
    let f = export("ntdll.dll", c"wine_get_host_version");
    if f == 0 {
        return None;
    }
    let (mut sys, mut rel): (*const c_char, *const c_char) = (std::ptr::null(), std::ptr::null());
    // SAFETY: `void CDECL wine_get_host_version(const char **sysname, const char **release)`.
    unsafe { std::mem::transmute::<usize, unsafe extern "C" fn(*mut *const c_char, *mut *const c_char)>(f)(&mut sys, &mut rel) };
    Some(format!("{} {}", c_text(sys), c_text(rel)))
}

/// The Linux path of a Windows path under Wine (None on Windows, or if Wine cannot map it).
pub fn unix_path(windows_path: &str) -> Option<String> {
    let f = export("kernel32.dll", c"wine_get_unix_file_name");
    if f == 0 {
        return None;
    }
    let w: Vec<u16> = windows_path.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `char *CDECL wine_get_unix_file_name(LPCWSTR)`, the result is on the process
    // heap and the caller frees it.
    let p = unsafe { std::mem::transmute::<usize, unsafe extern "C" fn(*const u16) -> *mut c_char>(f)(w.as_ptr()) };
    if p.is_null() {
        return None;
    }
    let s = c_text(p);
    unsafe { HeapFree(GetProcessHeap(), 0, p as *mut c_void) };
    Some(s)
}

/// The mod's data folder (log, backups) as the player finds it: `%LOCALAPPDATA%\sekiro-coop`
/// on Windows, the Proton prefix path on Linux. For notices: short lines, no wrapping.
pub fn data_folder_hint() -> &'static str {
    if is_wine() {
        "steamapps/compatdata/814380/pfx/drive_c/\nusers/steamuser/AppData/Local/sekiro-coop"
    } else {
        "%LOCALAPPDATA%\\sekiro-coop"
    }
}

/// One log line: Windows, or the Wine version, the host OS and where the data folder is on Linux.
pub fn log_line() -> String {
    let Some(v) = wine_version() else {
        return "platform: Windows".to_owned();
    };
    let os = host_os().unwrap_or_else(|| "?".to_owned());
    let dir = std::env::var("LOCALAPPDATA").map(|d| format!("{d}\\sekiro-coop")).unwrap_or_default();
    let unix = unix_path(&dir).unwrap_or_else(|| "?".to_owned());
    format!("platform: Wine {v} on {os} (Proton / Linux / Steam Deck); data folder {dir} = {unix}")
}
