//! Lightweight crash diagnostics: zero cost until something goes wrong.
//!
//! * `breadcrumb(id)` records what the mod was doing (one atomic store).
//! * A vectored exception handler logs fatal-looking exceptions (access
//!   violation, illegal instruction, stack overflow) with module+offset and the
//!   last breadcrumb, at most `MAX_REPORTS` times, then lets the game continue
//!   its normal handling (we never swallow exceptions).
//! * A panic hook logs Rust panics.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::log;

static BREADCRUMB: AtomicUsize = AtomicUsize::new(0);
static REPORTS: AtomicU32 = AtomicU32::new(0);
const MAX_REPORTS: u32 = 10;

/// Static labels so recording a breadcrumb is a single pointer store.
pub fn breadcrumb(label: &'static str) {
    BREADCRUMB.store(label.as_ptr() as usize | (label.len() << 48), Ordering::Relaxed);
    prof_mark(label);
}

/// v8.61 frame profile: the time from one breadcrumb to the next is charged to the first, so
/// the 2 s tick line can name the slowest part of a long mod frame (live 2026-10-28: mod
/// frames of 10-45 ms in most ticks, the cause unnamed). Per thread: only the frame task
/// starts a profile (`prof_frame_start`), breadcrumbs on other threads only store the label.
struct Prof {
    on: bool,
    last: Option<(std::time::Instant, &'static str)>,
    /// (label, longest single span) since the last `prof_take`.
    max: Vec<(&'static str, std::time::Duration)>,
}

thread_local! {
    static PROF: std::cell::RefCell<Prof> = const { std::cell::RefCell::new(Prof { on: false, last: None, max: Vec::new() }) };
}

fn prof_mark(label: &'static str) {
    let _ = PROF.try_with(|p| {
        let Ok(mut p) = p.try_borrow_mut() else { return };
        if !p.on {
            return;
        }
        let now = std::time::Instant::now();
        if let Some((t, l)) = p.last.replace((now, label)) {
            prof_record(&mut p.max, l, now.duration_since(t));
        }
    });
}

fn prof_record(max: &mut Vec<(&'static str, std::time::Duration)>, label: &'static str, d: std::time::Duration) {
    match max.iter_mut().find(|(l, _)| *l == label) {
        Some((_, m)) => *m = (*m).max(d),
        None => max.push((label, d)),
    }
}

/// Frame task: the start of our frame (the time between frames is never charged).
pub fn prof_frame_start() {
    let _ = PROF.try_with(|p| {
        if let Ok(mut p) = p.try_borrow_mut() {
            p.on = true;
            p.last = Some((std::time::Instant::now(), "frame start (items, menus, gpuguard)"));
        }
    });
}

/// Frame task: the end of our frame (charges the last span, stops charging).
pub fn prof_frame_end() {
    let _ = PROF.try_with(|p| {
        if let Ok(mut p) = p.try_borrow_mut() {
            if let Some((t, l)) = p.last.take() {
                let d = t.elapsed();
                prof_record(&mut p.max, l, d);
            }
            p.on = false;
        }
    });
}

/// Frame task, with the tick line: the slowest spans since the last call (reset).
pub fn prof_take() -> Vec<(&'static str, std::time::Duration)> {
    PROF.try_with(|p| p.try_borrow_mut().map(|mut p| std::mem::take(&mut p.max)).unwrap_or_default()).unwrap_or_default()
}

/// "; slowest parts (longest single span): a 12.3 ms, b 4.0 ms" (top `n`), "" if none.
pub fn prof_line(mut spans: Vec<(&'static str, std::time::Duration)>, n: usize) -> String {
    spans.sort_by_key(|a| std::cmp::Reverse(a.1));
    let parts: Vec<String> = spans.iter().take(n).map(|(l, d)| format!("{l} {:.1} ms", d.as_secs_f64() * 1000.0)).collect();
    if parts.is_empty() { String::new() } else { format!("; slowest parts (longest single span): {}", parts.join(", ")) }
}

fn last_breadcrumb() -> &'static str {
    let v = BREADCRUMB.load(Ordering::Relaxed);
    if v == 0 {
        return "none";
    }
    let (ptr, len) = (v & ((1 << 48) - 1), v >> 48);
    unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(ptr as *const u8, len)) }
}

#[repr(C)]
struct ExceptionRecord {
    code: u32,
    flags: u32,
    record: *mut ExceptionRecord,
    address: *mut c_void,
    num_params: u32,
    info: [usize; 15],
}

#[repr(C)]
struct ExceptionPointers {
    record: *mut ExceptionRecord,
    context: *mut c_void,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn AddVectoredExceptionHandler(first: u32, handler: extern "system" fn(*mut ExceptionPointers) -> i32) -> *mut c_void;
    fn GetModuleHandleExW(flags: u32, addr: *const u16, module: *mut *mut c_void) -> i32;
    fn GetModuleFileNameW(module: *mut c_void, buf: *mut u16, len: u32) -> u32;
    fn SetUnhandledExceptionFilter(filter: Option<extern "system" fn(*mut ExceptionPointers) -> i32>) -> Option<extern "system" fn(*mut ExceptionPointers) -> i32>;
}

const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
const GET_MODULE_FROM_ADDRESS: u32 = 0x4 | 0x2; // FROM_ADDRESS | UNCHANGED_REFCOUNT

fn describe_address(addr: usize) -> String {
    let mut module: *mut c_void = std::ptr::null_mut();
    let ok = unsafe { GetModuleHandleExW(GET_MODULE_FROM_ADDRESS, addr as *const u16, &mut module) };
    if ok == 0 || module.is_null() {
        return format!("{addr:#x} (no module)");
    }
    let mut buf = [0u16; 260];
    let n = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let path = String::from_utf16_lossy(&buf[..n]);
    let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
    format!("{name}+{:#x}", addr - module as usize)
}

extern "system" fn handler(info: *mut ExceptionPointers) -> i32 {
    let rec = unsafe { &*(*info).record };
    let fatal = matches!(rec.code, 0xC000_0005 | 0xC000_001D | 0xC000_00FD | 0xC000_0096);
    if fatal && REPORTS.fetch_add(1, Ordering::Relaxed) < MAX_REPORTS {
        let mut msg = format!(
            "CRASH? code {:#x} at {} last mod action: {}",
            rec.code,
            describe_address(rec.address as usize),
            last_breadcrumb()
        );
        if rec.code == 0xC000_0005 && rec.num_params >= 2 {
            let kind = match rec.info[0] { 0 => "read", 1 => "write", 8 => "execute", _ => "?" };
            msg.push_str(&format!(" ({kind} of {:#x})", rec.info[1]));
        }
        log::line(&msg);
        // Win64 CONTEXT: Rcx 0x80, Rdx 0x88, Rsp 0x98. Stack words that point into the
        // game exe are return addresses (or leftovers): enough to name the caller.
        let ctx = unsafe { (*info).context } as usize;
        if ctx != 0 {
            let reg = |off: usize| unsafe { *((ctx + off) as *const usize) };
            let rsp = reg(0x98);
            let mut frames = Vec::new();
            for i in 0..96 {
                let w = unsafe { *((rsp + i * 8) as *const usize) };
                if w > 0x1_0000_0000 {
                    let d = describe_address(w);
                    if d.starts_with("sekiro.exe+") {
                        frames.push(format!("[{i}]{}", &d["sekiro.exe".len()..]));
                    }
                }
            }
            log::line(&format!("CRASH regs rcx {:#x} rdx {:#x} rsp {rsp:#x}; stack: {}", reg(0x80), reg(0x88), frames.join(" ")));
        }
    }
    EXCEPTION_CONTINUE_SEARCH
}

type TopFilter = extern "system" fn(*mut ExceptionPointers) -> i32;

/// The filter that was in place before ours (0 = none), called after our line.
static PREV_FILTER: AtomicUsize = AtomicUsize::new(0);

/// v8.52: the exception nobody handled, i.e. the crash that ends the game (the `CRASH?` lines
/// above are first-chance and may have been handled). One line, then the previous filter
/// (WER / the game's own) runs as before.
extern "system" fn unhandled(info: *mut ExceptionPointers) -> i32 {
    let rec = unsafe { &*(*info).record };
    log::line_nowait(&format!(
        "EVENT SESSION_END crash: unhandled exception {:#x} at {} (last mod action: {})",
        rec.code,
        describe_address(rec.address as usize),
        last_breadcrumb()
    ));
    match PREV_FILTER.load(Ordering::Relaxed) {
        0 => EXCEPTION_CONTINUE_SEARCH,
        p => {
            let prev: TopFilter = unsafe { std::mem::transmute(p) };
            prev(info)
        }
    }
}

pub fn install() {
    unsafe { AddVectoredExceptionHandler(1, handler) };
    let prev = unsafe { SetUnhandledExceptionFilter(Some(unhandled)) };
    PREV_FILTER.store(prev.map_or(0, |f| f as usize), Ordering::Relaxed);
    std::panic::set_hook(Box::new(|p| {
        log::line(&format!("PANIC: {p} (last mod action: {})", last_breadcrumb()));
    }));
    log::line("crash: diagnostics installed");
}

#[cfg(test)]
mod prof_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn the_slowest_spans_are_named_longest_first() {
        let ms = Duration::from_millis;
        let mut max = Vec::new();
        prof_record(&mut max, "livesync", ms(3));
        prof_record(&mut max, "persist", ms(9));
        prof_record(&mut max, "livesync", ms(31));
        prof_record(&mut max, "livesync", ms(2));
        prof_record(&mut max, "net frame", ms(1));
        assert_eq!(max.len(), 3);
        assert_eq!(prof_line(max, 2), "; slowest parts (longest single span): livesync 31.0 ms, persist 9.0 ms");
        assert_eq!(prof_line(Vec::new(), 3), "");
    }

    #[test]
    fn breadcrumbs_charge_only_inside_a_frame() {
        let _ = prof_take();
        breadcrumb("outside");
        breadcrumb("outside again");
        assert!(prof_take().is_empty(), "no frame started: nothing charged");
        prof_frame_start();
        breadcrumb("a");
        breadcrumb("b");
        prof_frame_end();
        breadcrumb("between frames");
        let labels: Vec<&str> = prof_take().into_iter().map(|(l, _)| l).collect();
        assert!(labels.contains(&"a") && labels.contains(&"b"), "{labels:?}");
        assert!(!labels.contains(&"between frames"), "{labels:?}");
    }
}
