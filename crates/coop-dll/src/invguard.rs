//! Inventory guard (2026-10-24, diagnostic only, NOT verified live; IMPLEMENTATION-NOTES §45).
//!
//! A friend's 200 Spirit Emblems (goods 1000, full id `0x400003E8`) vanished during a co-op
//! session. No mod code writes the inventory, so this module only WATCHES and never writes
//! game memory: it logs the emblem amount and the sen at join, at leave, around every
//! own-world restore and every mod-called rest, and every 60 s. A drop of more than 20 with
//! no rest and no plausible item use in between is logged as
//! `audit: SUSPECT inventory: ...` (the offline analyzer reads `audit:` lines) and shown as an
//! overlay notice.
//!
//! Storage box: the save keeps it as a second list right after the key item list (slot offset
//! ~0x987B4, carried amount ~0x8F950), but its place in `PlayerGameData` in memory is not
//! known [ARV]: the vendor `EquipInventoryData` only has the normal and key lists. Until it is
//! found the guard watches the carried amount and says `storage unknown` in each line.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::game::{self, read, read_ptr};
use crate::log;

/// Spirit Emblem: goods 1000.
pub const EMBLEM: u32 = 0x4000_03E8;
/// A drop bigger than this is suspect.
pub const DROP_LIMIT: u32 = 20;
const EVERY: Duration = Duration::from_secs(60);
const GAME_DATA_MAN: usize = 0x3D5AAC0;
const GDM_PLAYER_GAME_DATA: usize = 0x08;
/// `PlayerGameData.player_info.sen` (current money).
const PGD_SEN: usize =
    std::mem::offset_of!(sekiro::sprj::PlayerGameData, player_info) + std::mem::offset_of!(sekiro::sprj::PlayerInfo, sen);

/// What was read: carried emblems, storage emblems (None = unknown), sen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub carried: u32,
    pub storage: Option<u32>,
    pub sen: i32,
}

impl Sample {
    /// Carried plus storage when storage is known, else carried alone.
    pub fn total(&self) -> u32 {
        self.carried + self.storage.unwrap_or(0)
    }
}

/// The emblem total went down by more than `DROP_LIMIT` with no rest and no item use between.
pub fn storage_drop(before: u32, after: u32, rested: bool, used: bool) -> bool {
    !rested && !used && before > after && before - after > DROP_LIMIT
}

/// Item use explains a drop: the carried amount fell by a small step and the total fell by no
/// more than that step plus the limit. (Emblems are used from the carried list only.)
pub fn explained_by_use(before: Sample, after: Sample) -> bool {
    let carried_down = before.carried.saturating_sub(after.carried);
    let total_down = before.total().saturating_sub(after.total());
    carried_down > 0 && carried_down <= DROP_LIMIT && total_down <= carried_down + DROP_LIMIT
}

fn read_sample() -> Option<Sample> {
    let pgd = read_ptr(game::module_base() + GAME_DATA_MAN).and_then(|g| read_ptr(g + GDM_PLAYER_GAME_DATA))?;
    let carried = crate::audit::read_inventory()?.into_iter().find(|(id, _)| *id == EMBLEM).map_or(0, |(_, q)| q);
    let sen = read::<i32>(pgd + PGD_SEN)?;
    Some(Sample { carried, storage: None, sen })
}

/// Emblems / sen seen just before the mod's own death reload (see `snapshot_before_reload`).
static RELOAD_SNAP: Mutex<Option<Snap>> = Mutex::new(None);
/// The guard watches this long after the reload call (live 2026-10-26: the wipe landed 7 s after
/// the call and only after the loading screen, so an early "nothing lost" verdict was wrong).
const SNAP_WINDOW: Duration = Duration::from_secs(45);
/// No verdict before this (the load has not even started yet).
const SNAP_SETTLE: Duration = Duration::from_secs(3);
/// The loss must stay visible this long (a value read as 0 while data loads is not a loss).
const ZERO_HOLD: Duration = Duration::from_secs(2);
/// Sample period of the guard.
const SNAP_SAMPLE_EVERY: Duration = Duration::from_millis(500);

/// The values seen before the reload and the guard's own bookkeeping.
#[derive(Clone, Copy)]
struct Snap {
    sample: Sample,
    at: Instant,
    /// Since when the last samples showed a restorable loss.
    loss_since: Option<Instant>,
    last_read: Option<Instant>,
}

/// One step of the guard.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Keep watching.
    Wait,
    /// Give back these (emblems, sen).
    Restore(Option<u32>, Option<i32>),
    /// The window is over: nothing was lost.
    Done,
}

/// Pure decision: `age` since the reload call, `now` the current sample (None = unreadable),
/// `loss_for` how long a restorable loss has been visible without a break.
fn verdict(snap: Sample, now: Option<Sample>, age: Duration, loss_for: Option<Duration>) -> Verdict {
    if age > SNAP_WINDOW {
        return Verdict::Done;
    }
    if age < SNAP_SETTLE {
        return Verdict::Wait;
    }
    let Some(now) = now else { return Verdict::Wait };
    let (emblems, sen) = restore_plan(snap, now);
    if emblems.is_none() && sen.is_none() {
        return Verdict::Wait;
    }
    if loss_for.is_some_and(|d| d >= ZERO_HOLD) { Verdict::Restore(emblems, sen) } else { Verdict::Wait }
}
/// `PlayerGameData` +0x160: experience (read-only, logged for the death-reset research).
const PGD_XP: usize = 0x160;

fn pgd() -> Option<usize> {
    read_ptr(game::module_base() + GAME_DATA_MAN).and_then(|g| read_ptr(g + GDM_PLAYER_GAME_DATA))
}

/// The main player's PlayerGameData (read only).
pub fn player_game_data() -> Option<usize> {
    pgd()
}

/// What to give back after the reload: emblems only when now exactly 0 (and the snapshot had
/// some), sen only when now exactly 0, as half the snapshot (a normal death halves it).
fn restore_plan(snap: Sample, now: Sample) -> (Option<u32>, Option<i32>) {
    let emblems = (now.carried == 0 && snap.carried > 0).then_some(snap.carried);
    let sen = (now.sen == 0 && snap.sen > 0).then_some(snap.sen / 2);
    (emblems, sen)
}

/// Called right before the mod calls the game's reload with null arguments (0xA8F7E0), which
/// leaves carried emblems and sen at 0 (a vanilla death keeps emblems, halves sen).
pub fn snapshot_before_reload() {
    let Some(s) = read_sample() else {
        log::line("invguard: snapshot before mod death reload: inventory unreadable");
        return;
    };
    let xp = pgd().and_then(|p| read::<i32>(p + PGD_XP));
    log::line(&format!(
        "invguard: snapshot before mod death reload: emblems {}, sen {} (xp {xp:?})",
        s.carried, s.sen
    ));
    if let Ok(mut g) = RELOAD_SNAP.lock() {
        *g = Some(Snap { sample: s, at: Instant::now(), loss_since: None, last_read: None });
    }
}

/// Watches the amounts for `SNAP_WINDOW` after the mod's own reload and gives back what a wipe
/// took (only a loss that stays visible for `ZERO_HOLD`, never during a loading screen).
fn restore_after_reload() {
    let Some(mut snap) = RELOAD_SNAP.lock().ok().and_then(|g| *g) else { return };
    let save = |s: Snap| {
        if let Ok(mut g) = RELOAD_SNAP.lock() {
            *g = Some(s);
        }
    };
    let clear = || {
        if let Ok(mut g) = RELOAD_SNAP.lock() {
            *g = None;
        }
    };
    let age = snap.at.elapsed();
    if age > SNAP_WINDOW {
        let last = read_sample();
        log::line(&format!(
            "invguard: mod death reload: nothing lost within {} s (now emblems {}, sen {}), guard off",
            SNAP_WINDOW.as_secs(),
            last.map_or(-1, |s| s.carried as i64),
            last.map_or(-1, |s| i64::from(s.sen))
        ));
        clear();
        return;
    }
    if game::load_pending() {
        // Values read during a loading screen mean nothing: start the "visible loss" clock again.
        if snap.loss_since.take().is_some() {
            save(snap);
        }
        return;
    }
    if snap.last_read.is_some_and(|t| t.elapsed() < SNAP_SAMPLE_EVERY) {
        return;
    }
    snap.last_read = Some(Instant::now());
    let now = read_sample();
    let lost = now.is_some_and(|n| {
        let (e, s) = restore_plan(snap.sample, n);
        e.is_some() || s.is_some()
    });
    snap.loss_since = if lost { snap.loss_since.or(Some(Instant::now())) } else { None };
    let verdict = verdict(snap.sample, now, age, snap.loss_since.map(|t| t.elapsed()));
    let (Verdict::Restore(emblems, sen), Some(now)) = (verdict, now) else {
        save(snap);
        return;
    };
    let snap = snap.sample;
    clear();
    if !crate::tuning::RESTORE_LOST_ITEMS.load(std::sync::atomic::Ordering::Relaxed) {
        log::line(&format!(
            "invguard: mod death reload lost emblems/sen (snapshot {}/{}, now {}/{}); restore_lost_items = 0, not restored",
            snap.carried, snap.sen, now.carried, now.sen
        ));
        return;
    }
    let mut ok = true;
    if let Some(n) = emblems {
        ok = crate::keyitems::give_entries(&[[EMBLEM, n, u32::MAX, 2]]);
    }
    let mut sen_ok = true;
    if let Some(v) = sen {
        sen_ok = pgd().is_some_and(|p| game::write::<i32>(p + PGD_SEN, v));
    }
    log::line(&format!(
        "invguard: restored after mod death reload: emblems 0 -> {} (ok {ok}), sen 0 -> {} (ok {sen_ok}) (snapshot {}/{})",
        emblems.unwrap_or(0),
        sen.unwrap_or(0),
        snap.carried,
        snap.sen
    ));
}

#[derive(Default)]
struct State {
    last: Option<Sample>,
    rested: bool,
    at: Option<Instant>,
    connected: bool,
    storage_logged: bool,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// A rest of ours happened (restsync.rs `note`): a drop after it is not suspect.
pub fn note_rest() {
    if let Ok(mut g) = STATE.lock() {
        g.get_or_insert_with(State::default).rested = true;
    }
}

/// Reads and logs the amounts; compares with the last sample. Game thread.
pub fn checkpoint(why: &str) {
    let Some(now) = read_sample() else { return };
    let Ok(mut g) = STATE.lock() else { return };
    let st = g.get_or_insert_with(State::default);
    let storage = match now.storage {
        Some(s) => format!("storage {s}"),
        None => {
            if !st.storage_logged {
                st.storage_logged = true;
                log::line("invguard: storage box list not located in memory [ARV], watching carried emblems and sen only");
            }
            "storage unknown".to_string()
        }
    };
    log::line(&format!("invguard: emblems carried {} {storage}, sen {} ({why})", now.carried, now.sen));
    if let Some(prev) = st.last
        && storage_drop(prev.total(), now.total(), st.rested, explained_by_use(prev, now))
    {
        let text = format!("Spirit Emblems total {} -> {} ({why})", prev.total(), now.total());
        log::line(&format!("audit: SUSPECT inventory: {text}"));
        crate::bossrules::notice(format!("Spirit Emblems dropped {} -> {}: logged", prev.total(), now.total()), 10);
    }
    st.last = Some(now);
    st.rested = false;
    st.at = Some(Instant::now());
}

/// Every in-world frame: join / leave edges and the 60 s sample.
pub fn tick() {
    restore_after_reload();
    let link = crate::net::link();
    let connected = matches!(link, crate::net::Link::HostConnected | crate::net::Link::ClientConnected);
    let (edge, due) = {
        let Ok(mut g) = STATE.lock() else { return };
        let st = g.get_or_insert_with(State::default);
        let edge = (connected != st.connected).then_some(connected);
        if edge.is_some() {
            st.connected = connected;
        }
        (edge, st.at.is_none_or(|t| t.elapsed() >= EVERY))
    };
    match edge {
        Some(true) => checkpoint("join"),
        Some(false) => checkpoint("leave"),
        None if due => checkpoint("60 s"),
        None => {}
    }
    watch_carried();
    watch_revive_lock();
}

/// `PlayerGameData + 0x150` f32: "Revive Recast Point", the Resurrect lock (0 = free; RE-notes
/// NOTES §56, static IDA only). Resurrect goods: 1110 Temporary Regenerative Power, 1100
/// Regenerative Power, 2000 Resurrection (the ability).
const PGD_REVIVE_LOCK: usize = 0x150;
const REVIVE_GOODS: [u32; 3] = [1110, 1100, 2000];

/// v8.20: the Resurrect inputs in one string (diagnostic, read only): lock value and the three
/// goods amounts (`-` = not in the inventory).
pub fn resurrect_fields() -> String {
    let Some(pgd) = read_ptr(game::module_base() + GAME_DATA_MAN).and_then(|g| read_ptr(g + GDM_PLAYER_GAME_DATA)) else {
        return "resurrect inputs: no PlayerGameData".to_string();
    };
    let lock = read::<f32>(pgd + PGD_REVIVE_LOCK).map_or("?".to_string(), |v| format!("{v:.1}"));
    let inv = crate::audit::read_inventory();
    let goods: Vec<String> = REVIVE_GOODS
        .iter()
        .map(|g| {
            let n = inv.as_ref().and_then(|i| i.iter().find(|(id, _)| *id == 0x4000_0000 + g).map(|(_, q)| *q));
            format!("goods {g} = {}", n.map_or("-".to_string(), |q| q.to_string()))
        })
        .collect();
    format!("resurrect inputs: PGD+0x150 lock {lock}, {}", goods.join(", "))
}

/// v8.20: a change of the lock value is logged with the full dump (it rises on a Resurrect and
/// should fall on kills; this is how the friend's missing kill credit would show). Read twice a
/// second, at most 200 lines.
fn watch_revive_lock() {
    static WATCH: Mutex<(Option<Instant>, Option<u32>, u32)> = Mutex::new((None, None, 0));
    let Ok(mut w) = WATCH.lock() else { return };
    if w.0.is_some_and(|t| t.elapsed() < Duration::from_millis(500)) {
        return;
    }
    w.0 = Some(Instant::now());
    let Some(pgd) = read_ptr(game::module_base() + GAME_DATA_MAN).and_then(|g| read_ptr(g + GDM_PLAYER_GAME_DATA)) else { return };
    let Some(now) = read::<f32>(pgd + PGD_REVIVE_LOCK).map(f32::to_bits) else { return };
    let changed = w.1.is_some_and(|b| b != now);
    w.1 = Some(now);
    if changed && w.2 < 200 {
        w.2 += 1;
        drop(w);
        crate::statelog::resurrect_dump("lock changed");
    }
}

/// v8.15: every change of the carried Spirit Emblems, with its time (live 2026-10-25: they went
/// to 0 over deaths and were not restocked by rests, and the 60 s samples cannot say when).
/// Read twice a second, at most 300 lines a session.
fn watch_carried() {
    static WATCH: Mutex<(Option<Instant>, Option<u32>, u32)> = Mutex::new((None, None, 0));
    let Ok(mut w) = WATCH.lock() else { return };
    if w.0.is_some_and(|t| t.elapsed() < Duration::from_millis(500)) {
        return;
    }
    w.0 = Some(Instant::now());
    let Some(now) = read_sample() else { return };
    if let Some(before) = w.1
        && before != now.carried
        && w.2 < 300
    {
        w.2 += 1;
        log::line(&format!("invguard: emblems carried {before} -> {} (sen {})", now.carried, now.sen));
    }
    w.1 = Some(now.carried);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_drop_table() {
        // (before, after, rested, used, expected)
        for (b, a, r, u, want) in [
            (200, 0, false, false, true),
            (200, 179, false, false, true),
            (200, 180, false, false, false),
            (200, 190, false, false, false),
            (200, 0, true, false, false),
            (200, 0, false, true, false),
            (0, 200, false, false, false),
            (5, 5, false, false, false),
        ] {
            assert_eq!(storage_drop(b, a, r, u), want, "{b} -> {a} rested {r} used {u}");
        }
    }

    #[test]
    fn use_explains_small_carried_drop_only() {
        let s = |c, st| Sample { carried: c, storage: Some(st), sen: 0 };
        assert!(explained_by_use(s(10, 200), s(9, 200)));
        assert!(!explained_by_use(s(10, 200), s(10, 0)));
        assert!(!explained_by_use(s(10, 200), s(9, 0)));
        // A deposit moves carried to storage: total unchanged, nothing to flag.
        assert!(!storage_drop(210, 210, false, explained_by_use(s(10, 200), s(0, 210))));
        // All carried lost (200 -> 0), no storage: not a use.
        let a = Sample { carried: 200, storage: None, sen: 0 };
        let b = Sample { carried: 0, storage: None, sen: 0 };
        assert!(!explained_by_use(a, b));
        assert!(storage_drop(a.total(), b.total(), false, explained_by_use(a, b)));
    }

    #[test]
    fn restore_plan_cases() {
        let m = |c, sen| Sample { carried: c, storage: None, sen };
        assert_eq!(restore_plan(m(15, 303), m(0, 0)), (Some(15), Some(151)));
        assert_eq!(restore_plan(m(15, 303), m(15, 151)), (None, None));
        assert_eq!(restore_plan(m(0, 0), m(0, 0)), (None, None));
        assert_eq!(restore_plan(m(15, 303), m(0, 151)), (Some(15), None));
    }

    #[test]
    fn the_guard_waits_for_a_loss_that_stays_and_ignores_early_values() {
        let m = |c, sen| Sample { carried: c, storage: None, sen };
        let snap = m(11, 9);
        let s = Duration::from_secs;
        // Too early, even with a loss on screen.
        assert_eq!(verdict(snap, Some(m(0, 0)), s(2), Some(s(5))), Verdict::Wait);
        // The old bug: the values still look fine 5 s after the call, so keep watching.
        assert_eq!(verdict(snap, Some(m(11, 5)), s(5), None), Verdict::Wait);
        // 7 s: the loss shows, but it has not held long enough.
        assert_eq!(verdict(snap, Some(m(0, 0)), s(7), Some(Duration::from_millis(500))), Verdict::Wait);
        // Held for 2 s: give back (sen half of the snapshot, emblems as they were).
        assert_eq!(verdict(snap, Some(m(0, 0)), s(9), Some(s(2))), Verdict::Restore(Some(11), Some(4)));
        // Unreadable sample: wait.
        assert_eq!(verdict(snap, None, s(9), Some(s(3))), Verdict::Wait);
        // Nothing lost for the whole window: done.
        assert_eq!(verdict(snap, Some(m(11, 5)), s(46), None), Verdict::Done);
        // A snapshot with nothing in it never restores.
        assert_eq!(verdict(m(0, 0), Some(m(0, 0)), s(9), Some(s(9))), Verdict::Wait);
    }
}
