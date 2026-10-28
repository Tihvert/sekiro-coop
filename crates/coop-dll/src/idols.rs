//! Idol activation sync (2026-10-24, NOT verified live).
//!
//! An idol is "lit" by an entry in NS_SPRJ::FrpgNetBonfireDb (`*(base+0x3D5A6A8)+0xBC0`),
//! not by its event flag. A friend's unlock flag (synced by world.rs, or copied raw by the
//! join snapshot / own-world restore) never creates that entry, so the idol stayed "unlit"
//! and resting there played the first-light scene again. Here the entry is created through
//! the game's own functions:
//!
//! - `0x14076E080(u32 entity) -> bool` IsBonfireLit.
//! - `0x14076EE60(u32 entity, bool net)` find-or-create the entry and light it (`net = false`:
//!   no packet).
//! - `base+0x3D5A1B4` (u8) refresh byte, set to 1 after lighting.
//! - Fallback A: `u32 entity` into `base+0x3AFEB88`; the next frame the vanilla idol-object
//!   path lights it (only when that idol is loaded). Only tried when the slot is free.
//!
//! Two feeders, both drained on the game thread in `tick` (World::on_frame): a queue of idols
//! whose unlock flag a friend just set (`note_flag_on`), and a reconcile every 5 s (and once
//! after each load, after the join snapshot and after an own-world restore reload) that lights
//! every idol whose flag is on but which is not lit, at most 20 per tick.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::game::{self, module_base, read_ptr};
use crate::log;

const DB_PTR_RVA: usize = 0x3D5A6A8;
const DB_ENTRIES: usize = 0xBC0;
const IS_LIT_RVA: usize = 0x76E080;
const LIGHT_RVA: usize = 0x76EE60;
const REFRESH_RVA: usize = 0x3D5A1B4;
#[allow(dead_code)]
const FALLBACK_A_RVA: usize = 0x3AFEB88;

/// In the world at least this long before anything is touched.
const MIN_IN_WORLD: Duration = Duration::from_secs(2);
const RECONCILE_EVERY: Duration = Duration::from_secs(5);
const RECONCILE_CAP: usize = 20;
const RETRY_LOG_EVERY: Duration = Duration::from_secs(5);

/// Entities waiting to be lit (a friend's unlock flag just went on).
static QUEUE: Mutex<Vec<u32>> = Mutex::new(Vec::new());
/// A reconcile is wanted at the next safe tick (join snapshot, own-world restore, a load).
static RECONCILE_WANTED: AtomicBool = AtomicBool::new(false);

struct State {
    last_reconcile: Option<Instant>,
    prev_in_world: Duration,
    last_retry_log: Option<Instant>,
}

static STATE: Mutex<State> = Mutex::new(State { last_reconcile: None, prev_in_world: Duration::ZERO, last_retry_log: None });

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|p| p.into_inner())
}

fn queue() -> std::sync::MutexGuard<'static, Vec<u32>> {
    QUEUE.lock().unwrap_or_else(|p| p.into_inner())
}

/// (idol entity, unlock flag) of every BonfireWarpParam row (same list as ownworld's).
fn rows() -> Option<&'static Vec<(i32, u32)>> {
    use fromsoftware_shared::FromStatic;
    use sekiro::param::{BONFIRE_WARP_PARAM_ST, ParamDef};
    use sekiro::sprj::SoloParamRepository;
    static ROWS: OnceLock<Vec<(i32, u32)>> = OnceLock::new();
    if let Some(r) = ROWS.get() {
        return Some(r);
    }
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    let file = repo.params().find(|p| p.struct_name() == BONFIRE_WARP_PARAM_ST::NAME).map(|p| &*p.data)?;
    let v: Vec<(i32, u32)> = unsafe { file.rows::<BONFIRE_WARP_PARAM_ST>() }
        .filter(|(_, r)| r.eventflag_id() > 0 && r.bonfire_entity_id() > 0)
        .map(|(_, r)| (r.bonfire_entity_id(), r.eventflag_id() as u32))
        .collect();
    (!v.is_empty()).then(|| ROWS.get_or_init(|| v))
}

fn entity_of(rows: &[(i32, u32)], flag: u32) -> Option<u32> {
    rows.iter().find(|(_, f)| *f == flag).map(|(e, _)| *e as u32)
}

fn flag_of(rows: &[(i32, u32)], entity: u32) -> Option<u32> {
    rows.iter().find(|(e, _)| *e as u32 == entity).map(|(_, f)| *f)
}

/// world.rs: a friend's flag `flag` was just set ON. Queues the idol if it is an unlock flag.
pub fn note_flag_on(flag: u32) {
    let Some(rows) = rows() else { return };
    let Some(e) = entity_of(rows, flag) else { return };
    let mut q = queue();
    if !q.contains(&e) && q.len() < 256 {
        q.push(e);
    }
}

/// A reconcile at the next safe tick (after the join snapshot / an own-world restore).
pub fn request_reconcile() {
    RECONCILE_WANTED.store(true, Ordering::Relaxed);
}

/// Pure guard: why nothing may be touched now. `Ok` = go.
fn guard(db_ready: bool, flags_ready: bool, loading: bool, in_world_for: Duration) -> Result<(), &'static str> {
    if !db_ready {
        return Err("DB not ready");
    }
    if !flags_ready {
        return Err("event flags not ready");
    }
    if loading {
        return Err("loading");
    }
    if in_world_for < MIN_IN_WORLD {
        return Err("just loaded");
    }
    Ok(())
}

/// Pure: the idols to light now: flag on, not lit, at most `cap`, in row order, no duplicates.
fn to_light(rows: &[(i32, u32)], flag_on: impl Fn(u32) -> bool, is_lit: impl Fn(u32) -> bool, cap: usize) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for &(e, f) in rows {
        if out.len() >= cap {
            break;
        }
        let e = e as u32;
        if e == 0 || out.contains(&e) || !flag_on(f) || is_lit(e) {
            continue;
        }
        out.push(e);
    }
    out
}

fn db_ready() -> bool {
    read_ptr(module_base() + DB_PTR_RVA).and_then(|db| read_ptr(db + DB_ENTRIES)).is_some_and(|p| p != 0)
}

fn flags_ready() -> bool {
    game::event_flag(1).is_some()
}

fn is_lit(entity: u32) -> bool {
    let f: extern "C" fn(u32) -> bool = unsafe { std::mem::transmute(module_base() + IS_LIT_RVA) };
    f(entity)
}

/// Lights the idol's bonfire entry without a packet. `Ok(true)` = lit now, `Ok(false)` = was
/// lit already. Game thread only.
pub fn ensure_lit(entity: u32) -> Result<bool, &'static str> {
    guard(db_ready(), flags_ready(), game::load_pending(), Duration::MAX)?;
    if is_lit(entity) {
        return Ok(false);
    }
    let light: extern "C" fn(u32, bool) = unsafe { std::mem::transmute(module_base() + LIGHT_RVA) };
    light(entity, false);
    game::write(module_base() + REFRESH_RVA, 1u8);
    if is_lit(entity) {
        return Ok(true);
    }
    // Fallback A (the vanilla latch at FALLBACK_A_RVA) is not used: the same slot carries a
    // real first light, and for an idol that is not loaded the value would linger. Log only,
    // so a live run shows whether the direct call is enough.
    Err("still unlit after the call")
}

fn light_logged(entity: u32, flag: u32, why: &str) -> bool {
    match ensure_lit(entity) {
        Ok(true) => {
            log::line(&format!("idols: lit idol {entity} (flag {flag}) {why} (lit = true)"));
            true
        }
        Ok(false) => false,
        Err(e) => {
            log::line(&format!("idols: {entity} still unlit after the call ({e})"));
            false
        }
    }
}

/// Per frame from World::on_frame (game thread, connected).
pub fn tick(in_world_for: Duration) {
    let Some(rows) = rows() else { return };
    let now = Instant::now();
    let (loaded, due) = {
        let mut st = state();
        let loaded = in_world_for < st.prev_in_world;
        st.prev_in_world = in_world_for;
        (loaded, st.last_reconcile.is_none_or(|t| now.duration_since(t) >= RECONCILE_EVERY))
    };
    if loaded {
        // A load: reconcile again once the world is settled.
        RECONCILE_WANTED.store(true, Ordering::Relaxed);
    }
    let queued = !queue().is_empty();
    let wanted = RECONCILE_WANTED.load(Ordering::Relaxed);
    if !queued && !wanted && !due {
        return;
    }
    if let Err(why) = guard(db_ready(), flags_ready(), game::load_pending(), in_world_for) {
        if queued || wanted {
            let mut st = state();
            if st.last_retry_log.is_none_or(|t| now.duration_since(t) >= RETRY_LOG_EVERY) {
                st.last_retry_log = Some(now);
                log::line(&format!("idols: {why}, retry"));
            }
        }
        return;
    }
    if queued {
        let q: Vec<u32> = std::mem::take(&mut *queue());
        for e in q {
            light_logged(e, flag_of(rows, e).unwrap_or(0), "from another player's unlock");
        }
    }
    if wanted || due {
        RECONCILE_WANTED.store(false, Ordering::Relaxed);
        state().last_reconcile = Some(now);
        let todo = to_light(rows, |f| game::event_flag(f) == Some(true), is_lit, RECONCILE_CAP);
        if !todo.is_empty() {
            let lit: Vec<u32> = todo.iter().copied().filter(|&e| light_logged(e, flag_of(rows, e).unwrap_or(0), "by reconcile")).collect();
            log::line(&format!("idols: reconcile lit {} idols {lit:?}", lit.len()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROWS: [(i32, u32); 4] = [(1100000, 11000), (1100001, 11001), (1100002, 11002), (1100003, 11003)];

    #[test]
    fn lights_flag_on_and_unlit_only() {
        let on = |f: u32| f == 11000 || f == 11001 || f == 11002;
        let lit = |e: u32| e == 1100001;
        assert_eq!(to_light(&ROWS, on, lit, 20), vec![1100000, 1100002]);
    }

    #[test]
    fn flag_off_and_lit_are_skipped() {
        assert!(to_light(&ROWS, |_| false, |_| false, 20).is_empty());
        assert!(to_light(&ROWS, |_| true, |_| true, 20).is_empty());
    }

    #[test]
    fn cap_limits_the_batch() {
        assert_eq!(to_light(&ROWS, |_| true, |_| false, 2), vec![1100000, 1100001]);
        assert!(to_light(&ROWS, |_| true, |_| false, 0).is_empty());
    }

    #[test]
    fn no_row_for_flag() {
        assert_eq!(entity_of(&ROWS, 99999), None);
        assert_eq!(entity_of(&ROWS, 11002), Some(1100002));
        assert_eq!(flag_of(&ROWS, 1100003), Some(11003));
        assert!(to_light(&[], |_| true, |_| false, 20).is_empty());
    }

    #[test]
    fn guard_blocks_until_everything_is_ready() {
        let ok = Duration::from_secs(3);
        assert_eq!(guard(false, true, false, ok), Err("DB not ready"));
        assert_eq!(guard(true, false, false, ok), Err("event flags not ready"));
        assert_eq!(guard(true, true, true, ok), Err("loading"));
        assert_eq!(guard(true, true, false, Duration::from_millis(1999)), Err("just loaded"));
        assert_eq!(guard(true, true, false, MIN_IN_WORLD), Ok(()));
    }
}
