//! Minimal append-only file log; no dependencies so Phase 0 stays tiny.

use std::fs::{self, File, OpenOptions};
use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn path() -> PathBuf {
    // Unit tests log to temp: in the real log their lines read as live spectate/warp events.
    #[cfg(test)]
    let base = std::env::temp_dir();
    #[cfg(not(test))]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("sekiro-coop").join("log.txt")
}

/// Above this the log is moved to log.old.txt (checked once per game start),
/// so an append-only log cannot fill the disk over weeks of play.
const MAX_LOG_BYTES: u64 = 8 << 20;

/// The open log (v8.6): opened once, not per line. Open + close per line ran on the game
/// thread (create_dir_all, CreateFile, the virus scanner's look at every close) for each of
/// the ~7 lines a second. Unbuffered, so a crash loses nothing; std shares read / write /
/// delete, so the uploader can copy it while the game runs.
static FILE: Mutex<Option<File>> = Mutex::new(None);

fn open() -> Option<File> {
    let path = path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = fs::rename(&path, path.with_file_name("log.old.txt"));
    }
    OpenOptions::new().create(true).append(true).open(&path).ok()
}

/// Epoch of the last `clock:` line (0 = none yet).
static LAST_CLOCK: AtomicU64 = AtomicU64::new(0);
/// A `clock:` line this often, so epoch values can be turned into wall-clock time anywhere in a long log.
const CLOCK_EVERY_SECS: u64 = 600;

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn write_raw(secs: u64, msg: &str) {
    // The whole line is built before the lock and goes out in ONE write: `writeln!` on an
    // unbuffered File issued a WriteFile per fragment (prefix, text, newline).
    let mut buf = String::with_capacity(msg.len() + 24);
    let _ = writeln!(buf, "[{secs}] {msg}");
    let mut g = FILE.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = open();
    }
    if let Some(f) = g.as_mut()
        && f.write_all(buf.as_bytes()).is_err()
    {
        // Deleted or moved under us: open it again on the next line.
        *g = None;
    }
}

/// One line that never waits for the log lock (v8.52): at process exit (loader lock held,
/// the other threads already gone, one of them may have died holding the lock) and in the
/// unhandled-exception filter. Busy lock: the line goes through a handle of its own.
pub fn line_nowait(msg: &str) {
    let mut buf = String::with_capacity(msg.len() + 24);
    let _ = writeln!(buf, "[{}] {msg}", now_secs());
    if let Ok(mut g) = FILE.try_lock()
        && let Some(f) = g.as_mut()
    {
        let _ = f.write_all(buf.as_bytes());
        return;
    }
    if let Ok(mut f) = OpenOptions::new().append(true).open(path()) {
        let _ = f.write_all(buf.as_bytes());
    }
}

pub fn line(msg: &str) {
    if muted(msg) {
        return;
    }
    let secs = now_secs();
    // Wall-clock reference first: before every F11 mark and every 10 minutes.
    if msg.starts_with("MARK F11") || secs.saturating_sub(LAST_CLOCK.load(Ordering::Relaxed)) >= CLOCK_EVERY_SECS {
        clock_line(secs);
    }
    write_raw(secs, msg);
}

/// `EVENT <kind> <detail>`: the compact timeline for offline analysis. Never rate limited.
/// Planned kinds (other modules adopt it later): BOSS_START, BOSS_END, DEATH, WIPE, REST, LOAD, RELEASE.
/// Already used: SESSION_START.
pub fn event(kind: &str, detail: &str) {
    let secs = now_secs();
    if detail.is_empty() {
        write_raw(secs, &format!("EVENT {kind}"));
    } else {
        write_raw(secs, &format!("EVENT {kind} {detail}"));
    }
}

/// Seconds east of UTC of the local time zone now (0 if it cannot be read).
#[cfg(windows)]
pub fn local_utc_offset_secs() -> i64 {
    #[repr(C)]
    struct TimeZoneInformation {
        bias: i32,
        standard_name: [u16; 32],
        standard_date: [u16; 8],
        standard_bias: i32,
        daylight_name: [u16; 32],
        daylight_date: [u16; 8],
        daylight_bias: i32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetTimeZoneInformation(tz: *mut TimeZoneInformation) -> u32;
    }
    // SAFETY: plain struct of integers, filled by the call.
    let mut tz: TimeZoneInformation = unsafe { std::mem::zeroed() };
    let id = unsafe { GetTimeZoneInformation(&mut tz) };
    if id == 0xFFFF_FFFF {
        return 0;
    }
    // id 2 = daylight saving in effect. Windows bias is minutes WEST of UTC.
    let extra = if id == 2 { tz.daylight_bias } else { tz.standard_bias };
    -(i64::from(tz.bias) + i64::from(extra)) * 60
}

#[cfg(not(windows))]
pub fn local_utc_offset_secs() -> i64 {
    0
}

/// `HH:MM:SS` of a unix time in a zone `offset` seconds east of UTC.
pub fn hms(epoch: u64, offset: i64) -> String {
    let day = (epoch as i64 + offset).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", day / 3600, day / 60 % 60, day % 60)
}

/// `+hh:mm` / `-hh:mm`.
pub fn offset_label(offset: i64) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    let m = offset.abs() / 60;
    format!("{sign}{:02}:{:02}", m / 60, m % 60)
}

/// Local wall-clock time now, `HH:MM:SS`.
pub fn local_hms() -> String {
    hms(now_secs(), local_utc_offset_secs())
}

fn clock_line(secs: u64) {
    LAST_CLOCK.store(secs, Ordering::Relaxed);
    let off = local_utc_offset_secs();
    write_raw(secs, &format!("clock: epoch {secs} = local {} (UTC offset {})", hms(secs, off), offset_label(off)));
    muted_summary(secs);
}

/// First lines of a run: mod version, build id, wall clock. Called once at DLL start.
pub fn session_header() {
    let secs = now_secs();
    let built: u64 = env!("COOP_BUILD_EPOCH").parse().unwrap_or(0);
    let off = local_utc_offset_secs();
    write_raw(secs, "==== sekiro-coop session start ====");
    write_raw(
        secs,
        &format!(
            "version: sekiro_coop v{} build {} (built epoch {built} = local {})",
            crate::ui::MOD_VERSION,
            env!("COOP_GIT_HASH"),
            hms(built, off)
        ),
    );
    clock_line(secs);
    event("SESSION_START", &format!("build={}", env!("COOP_GIT_HASH")));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hms_formats_with_offset() {
        let midnight = 1_793_059_200 - 1_793_059_200 % 86_400;
        assert_eq!(hms(midnight + 86_399 - 20, 0), "23:59:39");
        assert_eq!(hms(midnight, 7200), "02:00:00");
        // Crosses midnight backwards.
        assert_eq!(hms(midnight + 60, -3600), "23:01:00");
        assert_eq!(hms(0, 0), "00:00:00");
    }

    #[test]
    fn offset_labels() {
        assert_eq!(offset_label(0), "+00:00");
        assert_eq!(offset_label(7200), "+02:00");
        assert_eq!(offset_label(-12_600), "-03:30");
        assert_eq!(offset_label(19_800), "+05:30");
    }

    #[test]
    fn local_offset_is_sane() {
        let o = local_utc_offset_secs();
        assert!((-14 * 3600..=14 * 3600).contains(&o));
    }
}

/// Log categories switched off (`log_off` in the settings, v8.53): the first word of a line up to
/// `:`, `[`, `(` or a space, lower case ("gpuguard", "native", "t" for the T position lines).
/// EVENT lines and the `clock:` / session header lines never pass through here.
static MUTED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static ANY_MUTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Lines dropped per category since the last `clock:` line.
static DROPPED: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

/// The category of a line: its first word, lower case.
pub fn category(msg: &str) -> String {
    let end = msg.find([':', '[', '(', ' ']).unwrap_or(msg.len());
    msg[..end].to_ascii_lowercase()
}

/// Parses a `log_off` value ("observe, hkb, native: remote moved") into entries: a bare word is a
/// category, an entry with a `:` or a space is a line prefix (one line type of a category).
pub fn parse_categories(value: &str) -> Vec<String> {
    value.split([',', ';']).map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect()
}

/// The muted entry a line falls under, if any (see `parse_categories`).
fn muted_entry(entries: &[String], msg: &str) -> Option<String> {
    let cat = category(msg);
    entries.iter().find(|e| **e == cat || (e.contains([':', ' ']) && msg.len() >= e.len() && msg.as_bytes()[..e.len()].eq_ignore_ascii_case(e.as_bytes()))).cloned()
}

/// Sets the muted categories (settings.rs, every reload). A change is logged once.
pub fn set_muted(cats: Vec<String>) {
    let mut g = MUTED.lock().unwrap_or_else(|e| e.into_inner());
    if *g == cats {
        return;
    }
    ANY_MUTED.store(!cats.is_empty(), Ordering::Relaxed);
    *g = cats;
    let list = if g.is_empty() { "none".to_string() } else { g.join(", ") };
    drop(g);
    write_raw(now_secs(), &format!("log: categories off: {list} (settings log_off; counts in the clock lines)"));
}

fn muted(msg: &str) -> bool {
    if !ANY_MUTED.load(Ordering::Relaxed) || msg.starts_with("MARK") || msg.starts_with("CRASH") {
        return false;
    }
    let Some(cat) = muted_entry(&MUTED.lock().unwrap_or_else(|e| e.into_inner()), msg) else {
        return false;
    };
    let mut d = DROPPED.lock().unwrap_or_else(|e| e.into_inner());
    match d.iter_mut().find(|(c, _)| *c == cat) {
        Some((_, n)) => *n += 1,
        None => d.push((cat, 1)),
    }
    true
}

/// After each `clock:` line: how many lines each muted category dropped since the last one.
fn muted_summary(secs: u64) {
    let d = std::mem::take(&mut *DROPPED.lock().unwrap_or_else(|e| e.into_inner()));
    if d.is_empty() {
        return;
    }
    let parts: Vec<String> = d.iter().map(|(c, n)| format!("{c} {n}")).collect();
    write_raw(secs, &format!("log: muted lines since the last clock line: {}", parts.join(", ")));
}

#[cfg(test)]
mod category_tests {
    use super::*;

    #[test]
    fn categories_are_the_first_word() {
        assert_eq!(category("gpuguard: 0 s: 804 material uploads"), "gpuguard");
        assert_eq!(category("native[after 3 s]: Net chr"), "native");
        assert_eq!(category("T client ms=1 P=1,2,3"), "t");
        assert_eq!(category("netai send 4"), "netai");
        assert_eq!(category("ownfill (engagement): 3"), "ownfill");
        assert_eq!(parse_categories(" Observe, hkb ;t, native: remote moved"), vec!["observe", "hkb", "t", "native: remote moved"]);
        let e = parse_categories("hkb, native: remote moved");
        assert_eq!(muted_entry(&e, "native: remote moved 1.6 m off our pin").as_deref(), Some("native: remote moved"));
        assert_eq!(muted_entry(&e, "native: pin (10 s): 0 snaps"), None, "other native lines stay");
        assert_eq!(muted_entry(&e, "hkb: sent 1469 packets").as_deref(), Some("hkb"));
        assert_eq!(muted_entry(&e, "n"), None);
        assert!(parse_categories("").is_empty());
    }
}
