//! Rotating copies of the co-op save (2026-10-23, NOT verified live; IMPLEMENTATION-NOTES §22).
//!
//! The co-op save is `%APPDATA%\Sekiro\<steam id>\S0000.coop.sl2`: the game builds the folder
//! with `SHGetFolderPathW(CSIDL_APPDATA = 26)` + `"%s/Sekiro/%s/"` (`0x141151C60`, IDA
//! 2026-10-23), and the me3 profile renames the file (`savefile = "S0000.coop.sl2"`). We do
//! not ask Steam for the id: of the `S0000.coop.sl2` files under `%APPDATA%\Sekiro\*\`, the
//! one written last is the one this game uses [I: the game writes it on every autosave].
//!
//! Copies go to `%LOCALAPPDATA%\sekiro-coop\save-backups\S0000.coop.<yyyyMMdd-HHmmss>-<reason>.sl2`
//! at session start, session end, before an own-world restore and every 10 min in co-op. The
//! newest `KEEP` are kept; older ones are deleted (never one younger than 2 h, the last of each of the 3 previous days, the first of each of the last 3 days), only files matching that name pattern and
//! only inside that folder (our own managed folder). Everything runs on one worker thread:
//! the game thread only queues a request. The copy waits while the save was written in the
//! last 2 s (the autosave may still be writing), retries while the file is locked, and is
//! thrown away and retried when the save changed during the copy.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant, SystemTime};

use crate::log;

/// Backups kept in the folder (newest by the time stamp in the name).
pub const KEEP: usize = 10;
const SAVE_NAME: &str = "S0000.coop.sl2";
const PREFIX: &str = "S0000.coop.";
const SUFFIX: &str = ".sl2";
/// A save modified this recently may still be being written.
const QUIET_FOR: Duration = Duration::from_secs(2);
const RETRY_EVERY: Duration = Duration::from_secs(1);
/// Give up on one request after this long (locked file, save keeps changing).
const GIVE_UP_AFTER: Duration = Duration::from_secs(60);
/// While in co-op.
const PERIODIC: Duration = Duration::from_secs(600);

// ---------------------------------------------------------------- names (pure)

/// Local time as (year, month, day, hour, minute, second).
pub type Stamp = (u16, u16, u16, u16, u16, u16);

/// `S0000.coop.20261023-141502-session-start.sl2`. The reason is cut down to `[a-z0-9-]`.
pub fn backup_name(t: Stamp, reason: &str) -> String {
    let r: String = reason
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(24)
        .collect();
    let r = if r.is_empty() { "backup".to_string() } else { r };
    format!("{PREFIX}{:04}{:02}{:02}-{:02}{:02}{:02}-{r}{SUFFIX}", t.0, t.1, t.2, t.3, t.4, t.5)
}

/// The time stamp of one of our backup names (`yyyyMMdd-HHmmss`), None for any other file.
fn stamp_of(name: &str) -> Option<&str> {
    let rest = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    let stamp = rest.get(..15)?;
    let ok = stamp.bytes().enumerate().all(|(i, b)| if i == 8 { b == b'-' } else { b.is_ascii_digit() });
    // The reason follows after one more dash (the name `backup_name` makes).
    (ok && rest.as_bytes().get(15) == Some(&b'-') && rest.len() > 16).then_some(stamp)
}

/// Backups younger than this are never deleted (2026-10-24: the evidence of a recent loss).
pub const MIN_AGE_SECS: i64 = 2 * 3600;
/// Days kept: the last backup of each of this many days before today, and the first backup of
/// each of this many days ending today.
pub const DAYS_KEPT: i64 = 3;

/// Days since 1970-01-01 (proleptic Gregorian civil date).
fn day_number(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// (day number, seconds since the epoch of local time) of a `yyyyMMdd-HHmmss` stamp.
fn parse_stamp(s: &str) -> Option<(i64, i64)> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0..4)?, n(4..6)?, n(6..8)?, n(9..11)?, n(11..13)?, n(13..15)?);
    let day = day_number(y, mo, d);
    Some((day, day * 86_400 + h * 3600 + mi * 60 + se))
}

/// Which of `names` to delete, oldest first. Rotation as before (the newest `keep` of our
/// backups stay) except that these are never deleted: a backup younger than `MIN_AGE_SECS`, the
/// last backup of each of the `DAYS_KEPT` days before today, and the first backup of each of
/// today and the `DAYS_KEPT - 1` days before. `now` is local time. Names that are not ours are
/// never returned.
pub fn to_prune(names: &[String], keep: usize, now: Stamp) -> Vec<String> {
    let now_day = day_number(now.0 as i64, now.1 as i64, now.2 as i64);
    let now_secs = now_day * 86_400 + now.3 as i64 * 3600 + now.4 as i64 * 60 + now.5 as i64;
    let mut ours: Vec<(&str, &String, i64, i64)> = names
        .iter()
        .filter_map(|n| {
            let s = stamp_of(n)?;
            let (day, secs) = parse_stamp(s).unwrap_or((i64::MIN, i64::MIN));
            Some((s, n, day, secs))
        })
        .collect();
    // Newest first: the stamp, then the whole name, so the order is total.
    ours.sort_by(|a, b| b.0.cmp(a.0).then(b.1.cmp(a.1)));
    let mut protect = vec![false; ours.len()];
    let mut seen_last: Vec<i64> = Vec::new();
    for (i, &(_, _, day, secs)) in ours.iter().enumerate() {
        if day == i64::MIN {
            continue;
        }
        if now_secs - secs < MIN_AGE_SECS {
            protect[i] = true;
        }
        // Newest first, so the first one seen for a day is the last of that day.
        if day < now_day && now_day - day <= DAYS_KEPT && !seen_last.contains(&day) {
            seen_last.push(day);
            protect[i] = true;
        }
    }
    // Oldest first, so the first one seen for a day is the first of that day.
    let mut seen_first: Vec<i64> = Vec::new();
    for (i, &(_, _, day, _)) in ours.iter().enumerate().rev() {
        if day != i64::MIN && day <= now_day && now_day - day < DAYS_KEPT && !seen_first.contains(&day) {
            seen_first.push(day);
            protect[i] = true;
        }
    }
    let mut out: Vec<String> =
        ours.iter().enumerate().skip(keep).filter(|(i, _)| !protect[*i]).map(|(_, (_, n, _, _))| (*n).clone()).collect();
    out.reverse();
    out
}

// ---------------------------------------------------------------- paths

pub fn dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("sekiro-coop").join("save-backups")
}

/// The co-op save written last under `%APPDATA%\Sekiro\*\`.
fn find_save() -> Option<PathBuf> {
    let root = PathBuf::from(std::env::var_os("APPDATA")?).join("Sekiro");
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(&root).ok()?.flatten() {
        let p = e.path().join(SAVE_NAME);
        let Ok(t) = std::fs::metadata(&p).and_then(|m| m.modified()) else { continue };
        if best.as_ref().is_none_or(|(bt, _)| t > *bt) {
            best = Some((t, p));
        }
    }
    best.map(|(_, p)| p)
}

#[repr(C)]
#[derive(Default)]
struct SystemTimeW {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    ms: u16,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLocalTime(t: *mut SystemTimeW);
}

fn local_stamp() -> Stamp {
    let mut t = SystemTimeW::default();
    unsafe { GetLocalTime(&mut t) };
    (t.year, t.month, t.day, t.hour, t.minute, t.second)
}

// ---------------------------------------------------------------- worker

struct Job {
    ticket: u64,
    reason: String,
}

static SENDER: Mutex<Option<Sender<Job>>> = Mutex::new(None);
static NEXT: AtomicU64 = AtomicU64::new(1);
/// Highest ticket finished (copied, or given up on).
static DONE: AtomicU64 = AtomicU64::new(0);

/// Queues a copy of the co-op save. Never blocks: the worker thread does the work.
/// Returns the ticket for `done`.
pub fn request(reason: &str) -> u64 {
    let ticket = NEXT.fetch_add(1, Ordering::Relaxed);
    let Ok(mut s) = SENDER.lock() else { return ticket };
    if s.is_none() {
        let (tx, rx) = channel::<Job>();
        let spawned = std::thread::Builder::new().name("coop-save-backup".into()).spawn(move || {
            while let Ok(job) = rx.recv() {
                let r = std::panic::catch_unwind(|| run(&job));
                if r.is_err() {
                    log::line(&format!("backup: #{} ({}) panicked, skipped", job.ticket, job.reason));
                }
                DONE.fetch_max(job.ticket, Ordering::AcqRel);
            }
        });
        match spawned {
            Ok(_) => *s = Some(tx),
            Err(e) => {
                log::line(&format!("backup: no worker thread ({e}), backups are off"));
                DONE.fetch_max(ticket, Ordering::AcqRel);
                return ticket;
            }
        }
    }
    if let Some(tx) = s.as_ref()
        && tx.send(Job { ticket, reason: reason.to_string() }).is_err()
    {
        DONE.fetch_max(ticket, Ordering::AcqRel);
    }
    ticket
}

/// The request with this ticket has finished (successfully or not).
pub fn done(ticket: u64) -> bool {
    DONE.load(Ordering::Acquire) >= ticket
}

fn modified(p: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

fn run(job: &Job) {
    let t0 = Instant::now();
    let Some(save) = find_save() else {
        log::line(&format!("backup: #{} ({}): no {SAVE_NAME} under %APPDATA%\\Sekiro, nothing copied", job.ticket, job.reason));
        return;
    };
    let dir = dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::line(&format!("backup: #{} ({}): cannot create {}: {e}", job.ticket, job.reason, dir.display()));
        return;
    }
    let mut tries = 0u32;
    loop {
        tries += 1;
        if t0.elapsed() >= GIVE_UP_AFTER {
            log::line(&format!("backup: #{} ({}) gave up after {tries} tries ({:.0} s): {} stayed busy", job.ticket, job.reason, t0.elapsed().as_secs_f32(), save.display()));
            return;
        }
        let Some((before, len)) = modified(&save) else {
            std::thread::sleep(RETRY_EVERY);
            continue;
        };
        // The autosave may still be writing.
        if SystemTime::now().duration_since(before).is_ok_and(|age| age < QUIET_FOR) {
            std::thread::sleep(RETRY_EVERY);
            continue;
        }
        let mut name = backup_name(local_stamp(), &job.reason);
        if dir.join(&name).exists() {
            // Two requests in one second with the same reason.
            name = name.replace(SUFFIX, &format!("-{}{SUFFIX}", job.ticket));
        }
        let dest = dir.join(&name);
        let part = dir.join(format!("{name}.partial"));
        match std::fs::copy(&save, &part) {
            Ok(n) if modified(&save) == Some((before, len)) && n == len => {
                if let Err(e) = std::fs::rename(&part, &dest) {
                    log::line(&format!("backup: #{} ({}): rename to {} failed: {e}", job.ticket, job.reason, dest.display()));
                    let _ = std::fs::remove_file(&part);
                    return;
                }
                let pruned = prune(&dir);
                log::line(&format!(
                    "backup: {} -> {} ({n} B, {tries} tries, {:.0} ms){}",
                    save.display(),
                    dest.display(),
                    t0.elapsed().as_secs_f32() * 1000.0,
                    if pruned.is_empty() { String::new() } else { format!("; deleted the oldest {pruned:?}") }
                ));
                return;
            }
            Ok(_) => {
                // The save changed while we copied it: try again once it is quiet.
                let _ = std::fs::remove_file(&part);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                if tries == 1 || tries % 10 == 0 {
                    log::line(&format!("backup: #{} ({}): copy failed ({e}), retrying", job.ticket, job.reason));
                }
            }
        }
        std::thread::sleep(RETRY_EVERY);
    }
}

/// Deletes our oldest backups beyond `KEEP` (only our name pattern, only in our folder).
fn prune(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let names: Vec<String> = rd.flatten().filter(|e| e.path().is_file()).filter_map(|e| e.file_name().into_string().ok()).collect();
    let mut gone = Vec::new();
    for n in to_prune(&names, KEEP, local_stamp()) {
        if std::fs::remove_file(dir.join(&n)).is_ok() {
            gone.push(n);
        }
    }
    gone
}

// ---------------------------------------------------------------- session triggers

#[derive(Default)]
struct Watch {
    connected: bool,
    periodic_at: Option<Instant>,
    /// Autosave rate (v8.6, host stutter hunt): the save file, its last write time, writes
    /// seen since `saves_from`, and the last poll.
    save: Option<PathBuf>,
    save_mtime: Option<SystemTime>,
    saves: u32,
    saves_from: Option<Instant>,
    polled_at: Option<Instant>,
}

static WATCH: Mutex<Option<Watch>> = Mutex::new(None);

/// Every in-world frame (ownworld.rs `on_frame`): session start / end / every 10 min.
pub fn tick() {
    let link = crate::net::link();
    let connected = matches!(link, crate::net::Link::HostConnected | crate::net::Link::ClientConnected);
    let Ok(mut g) = WATCH.lock() else { return };
    let w = g.get_or_insert_with(Watch::default);
    let now = Instant::now();
    if connected != w.connected {
        w.connected = connected;
        let reason = if connected { "session-start" } else { "session-end" };
        let t = request(reason);
        log::line(&format!("backup: {reason} copy queued (#{t})"));
        w.periodic_at = Some(now);
    } else if connected && w.periodic_at.is_some_and(|t| now.duration_since(t) >= PERIODIC) {
        w.periodic_at = Some(now);
        let t = request("periodic");
        log::line(&format!("backup: periodic copy queued (#{t})"));
    }
    autosave_rate(w, now);
}

/// Once a second: did the game write the co-op save? One line a minute with the count (me3
/// showed ~43 save-file opens a minute on the host, live 2026-10-25; this compares players).
fn autosave_rate(w: &mut Watch, now: Instant) {
    if w.polled_at.is_some_and(|t| now.duration_since(t) < Duration::from_secs(1)) {
        return;
    }
    w.polled_at = Some(now);
    if w.save.is_none() {
        w.save = find_save();
    }
    let Some(t) = w.save.as_deref().and_then(modified).map(|(t, _)| t) else { return };
    if w.save_mtime.is_some_and(|old| old != t) {
        w.saves += 1;
    }
    w.save_mtime = Some(t);
    let from = *w.saves_from.get_or_insert(now);
    if now.duration_since(from) >= Duration::from_secs(60) {
        log::line(&format!("backup: the game wrote its save in {} of the last 60 seconds", w.saves));
        w.saves = 0;
        w.saves_from = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_carry_the_stamp_and_a_clean_reason() {
        let n = backup_name((2026, 10, 23, 9, 5, 7), "Session Start!");
        assert_eq!(n, "S0000.coop.20261023-090507-session-start-.sl2");
        assert_eq!(backup_name((2026, 1, 2, 3, 4, 5), "pre-restore"), "S0000.coop.20260102-030405-pre-restore.sl2");
        assert_eq!(backup_name((2026, 1, 2, 3, 4, 5), ""), "S0000.coop.20260102-030405-backup.sl2");
        assert_eq!(stamp_of(&n), Some("20261023-090507"));
    }

    #[test]
    fn foreign_files_are_never_ours() {
        for n in [
            "S0000.coop.sl2",
            "S0000.sl2",
            "S0000.coop.sl2.bak",
            "S0000.coop.20261023-090507.sl2",
            "S0000.coop.20261023-090507-x.sl2.partial",
            "S0000.coop.2026102X-090507-x.sl2",
            "S0000.coop.20261023_090507-x.sl2",
            "notes.txt",
        ] {
            assert_eq!(stamp_of(n), None, "{n}");
        }
    }

    #[test]
    fn prune_keeps_the_newest_and_ignores_other_files() {
        let mut names: Vec<String> = (0..13).map(|i| backup_name((2026, 10, 23, 10, i, 0), "periodic")).collect();
        names.push("S0000.coop.sl2".into());
        names.push("readme.txt".into());
        names.push("S0000.coop.20261023-100000-periodic.sl2.partial".into());
        // Out of order on purpose.
        names.reverse();
        let gone = to_prune(&names, 10, (2026, 11, 30, 12, 0, 0));
        assert_eq!(gone, (0..3).map(|i| backup_name((2026, 10, 23, 10, i, 0), "periodic")).collect::<Vec<_>>(), "the three oldest, oldest first");
        assert!(to_prune(&names, 20, (2026, 11, 30, 12, 0, 0)).is_empty());
        assert_eq!(to_prune(&names, 0, (2026, 11, 30, 12, 0, 0)).len(), 13);
        // Same second, different reasons: both counted, order still total.
        let two = vec![backup_name((2026, 1, 1, 0, 0, 0), "a"), backup_name((2026, 1, 1, 0, 0, 0), "b")];
        assert_eq!(to_prune(&two, 1, (2026, 11, 30, 12, 0, 0)), vec![backup_name((2026, 1, 1, 0, 0, 0), "a")]);
    }

    fn nm(d: u16, h: u16, m: u16) -> String {
        backup_name((2026, 10, d, h, m, 0), "x")
    }

    #[test]
    fn retention_protects_young_last_of_day_and_first_of_day() {
        // Now: 2026-10-24 14:07. keep = 2, so nearly everything is beyond the rotation.
        let now = (2026, 10, 24, 14, 7, 7);
        let names = vec![
            nm(20, 9, 0),   // 4 days ago: plain rotation, deleted
            nm(21, 9, 0),   // 3 days ago, first of day: only 3 days back counts for "last", deleted
            nm(21, 18, 0),  // 3 days ago, last of day: protected
            nm(22, 8, 0),   // first of day (2 days ago): protected
            nm(22, 12, 0),  // middle: deleted
            nm(22, 20, 0),  // last of the 22nd: protected
            nm(23, 7, 0),   // first of the 23rd: protected
            nm(23, 15, 0),  // middle: deleted
            nm(23, 23, 0),  // last of the 23rd: protected
            nm(24, 6, 0),   // first of today: protected
            nm(24, 9, 0),   // middle, 5 h old: deleted
            nm(24, 12, 30), // 1 h 37 min old: protected by age
            nm(24, 13, 0),  // newest 2 are kept by rotation
            nm(24, 14, 0),
        ];
        let gone = to_prune(&names, 2, now);
        assert_eq!(gone, vec![nm(20, 9, 0), nm(21, 9, 0), nm(22, 12, 0), nm(23, 15, 0), nm(24, 9, 0)]);
    }

    #[test]
    fn retention_still_rotates_when_nothing_is_protected() {
        // Old clutter more than 3 days back: plain rotation.
        let names: Vec<String> = (0..6).map(|i| nm(1, 10, i)).collect();
        let gone = to_prune(&names, 2, (2026, 10, 24, 14, 0, 0));
        assert_eq!(gone, (0..4).map(|i| nm(1, 10, i)).collect::<Vec<_>>());
    }

    #[test]
    fn day_numbers_cross_month_and_year() {
        assert_eq!(day_number(2026, 11, 1) - day_number(2026, 10, 31), 1);
        assert_eq!(day_number(2027, 1, 1) - day_number(2026, 12, 31), 1);
        assert_eq!(day_number(1970, 1, 1), 0);
    }
}
