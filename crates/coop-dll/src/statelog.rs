//! Player input-state logger (`[DEBUG] state_log = 1`, v7.8 test build; live 2026-10-24
//! "move only" lock: no menu, no attacks, walking works, cleared by death or by itself).
//!
//! Every 500 ms (game thread, our frame task) it reads, with safe reads only, the local
//! player's state that can gate attacks / menus and logs one `state:` line when any of it
//! changed: SpEffect ids on the player (added / removed; list `chr+0x11D0`, first node
//! `+0x08`, node id `+0x58`, next `+0x70`, RE-BACKSTAB.md), the ChrIns bit flags
//! `+0x1F40..+0x1F47` (NoHit / NoAttack / NoMove ..., WEB-PRIOR-ART §4.5), the data module
//! flags `+0x228`, `+0x1A10..+0x1A17`, team / char type, the menu mask (`0x8C58E0`), the
//! non-zero MenuMan state words (`MenuMan+0x20 + 4*i`, i < 256; 135 = return to title),
//! talk idle, the pause manager's paused byte, the load request, the current animation,
//! and the event flags written in the last 2 s (ours and the friend's, world.rs).
//! F11 (works with state_log = 1 even when debug_hotkeys = 0) writes the full state once.
//! Observes only: no write, no game call except the menu-mask getter and the flag getter
//! the rest of the mod already calls every frame.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::game::{self, module_base, read, read_bytes, read_ptr};
use crate::log;

/// `[DEBUG] state_log` (default on in this test build).
pub static ENABLED: AtomicBool = AtomicBool::new(true);

const EVERY: Duration = Duration::from_millis(500);
const FLAGS_WINDOW_MS: u64 = 2000;
const CHR_SP_LIST: usize = 0x11D0;
const SP_FIRST: usize = 0x08;
const SP_NODE_ID: usize = 0x58;
const SP_NODE_NEXT: usize = 0x70;
const SP_MAX: usize = 128;
const CHR_FLAGS: usize = 0x1F40;
const CHR_1A10: usize = 0x1A10;
const CHR_TYPE: usize = 0x70;
const CHR_TEAM: usize = 0x74;
const CHR_MODULES: usize = 0x1FF8;
const MOD_DATA: usize = 0x18;
const DATA_HP: usize = 0x130;
const DATA_FLAGS: usize = 0x228;
const MENU_MAN_RVA: usize = 0x3D67408;
const MENU_STATES: usize = 0x20;
/// The setter `0x1408C68B0(id, v)` accepts ids up to 0x3E7 [VAR, IDA `F:\Temp\ida_102`]:
/// 1000 slots, not 256 (live 2026-10-24 lock: no change in 0..255, so log them all).
const MENU_STATE_COUNT: usize = 1000;
/// ChrIns modules: ThrowModule `+0x88`, its node `+0x10`, node timer `+0x144`
/// (enemies.rs `throw_active`, `0xB4E9C0` / `0x802A60`).
const MOD_THROW: usize = 0x88;
const THROW_NODE: usize = 0x10;
const THROW_TIMER: usize = 0x144;
const PAUSE_MAN_RVA: usize = 0x3E9DA30;
const GAME_MAN_RVA: usize = 0x3D5C0D0;
const MAX_CHANGE_LINES: u32 = 4000;

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

#[derive(Clone, Default, PartialEq)]
struct Snap {
    sp: Vec<i32>,
    chr_flags: Option<[u8; 8]>,
    b1a10: Option<[u8; 8]>,
    data_flags: Option<u32>,
    team: Option<u8>,
    ctype: Option<i32>,
    menu_mask: Option<u32>,
    menu_states: Vec<(u16, i32)>,
    /// The player's ThrowModule: (node allocated, its timer > 0). A throw / deathblow
    /// left running on the player is one candidate for the "move and block only" lock.
    throw: Option<(bool, bool)>,
    talk_idle: bool,
    paused: Option<u8>,
    load_req: Option<u8>,
    anim: i32,
    alive: bool,
    /// The remo (cutscene) manager: flags `+0x144`, step `+0x40`, pending `+0xD0` (cutscene.rs).
    remo: Option<(u32, i32, u64)>,
}

struct State {
    at: Option<Instant>,
    last: Option<Snap>,
    flags: VecDeque<(u64, u32, bool, bool)>,
    lines: u32,
}

static STATE: Mutex<State> = Mutex::new(State { at: None, last: None, flags: VecDeque::new(), lines: 0 });

pub(crate) fn sp_ids(p: usize) -> Vec<i32> {
    let mut v = Vec::new();
    let Some(list) = read_ptr(p + CHR_SP_LIST) else { return v };
    let mut node = read_ptr(list + SP_FIRST);
    while let Some(n) = node {
        if v.len() >= SP_MAX {
            break;
        }
        if let Some(id) = read::<i32>(n + SP_NODE_ID) {
            v.push(id);
        }
        node = read_ptr(n + SP_NODE_NEXT).filter(|&x| x != n);
    }
    v.sort_unstable();
    v
}

/// The game's own Resurrect verdict from the player's SpEffects: `Some(true)` = offered (110090 on),
/// `Some(false)` = not offered (110093 without 110090), `None` = not decided yet (live 2026-10-25:
/// the signal comes 2-4 s after the death and matched the real state in every case).
pub fn resurrect_verdict(sp: &[i32]) -> Option<bool> {
    if sp.contains(&110090) {
        Some(true)
    } else if sp.contains(&110093) {
        Some(false)
    } else {
        None
    }
}

/// `resurrect_verdict` of the main player's current SpEffects (None = undecided or unreadable).
/// Reads game memory: call from the frame task, not from a game-thread hook.
pub fn player_resurrect_verdict() -> Option<bool> {
    let p = game::player()?.0;
    resurrect_verdict(&sp_ids(p))
}

/// The non-zero MenuMan state words of `raw` (little-endian i32 per id).
fn nonzero_states(raw: &[u8]) -> Vec<(u16, i32)> {
    raw.chunks_exact(4)
        .enumerate()
        .filter_map(|(i, b)| {
            let v = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            (v != 0).then_some((i as u16, v))
        })
        .collect()
}

fn throw_state(p: usize) -> Option<(bool, bool)> {
    let tm = read_ptr(p + CHR_MODULES).and_then(|m| read_ptr(m + MOD_THROW))?;
    let node = read::<usize>(tm + THROW_NODE)?;
    let running = node != 0 && read::<f32>(node + THROW_TIMER).is_some_and(|t| t > 0.0);
    Some((node != 0, running))
}

fn snap(anim: i32) -> Option<Snap> {
    let p = game::player()?.0;
    let data = read_ptr(p + CHR_MODULES).and_then(|m| read_ptr(m + MOD_DATA));
    let menu = read_ptr(module_base() + MENU_MAN_RVA);
    let mut menu_states = Vec::new();
    if let Some(m) = menu {
        let mut raw = vec![0u8; 4 * MENU_STATE_COUNT];
        if read_bytes(m + MENU_STATES, &mut raw) {
            menu_states = nonzero_states(&raw);
        }
    }
    Some(Snap {
        sp: sp_ids(p),
        chr_flags: read::<[u8; 8]>(p + CHR_FLAGS),
        b1a10: read::<[u8; 8]>(p + CHR_1A10),
        data_flags: data.and_then(|d| read::<u32>(d + DATA_FLAGS)),
        team: read::<u8>(p + CHR_TEAM),
        ctype: read::<i32>(p + CHR_TYPE),
        menu_mask: game::menu_mask(),
        menu_states,
        throw: throw_state(p),
        talk_idle: game::talk_idle(),
        paused: read_ptr(module_base() + PAUSE_MAN_RVA).and_then(|pm| read::<u8>(pm + 8)),
        load_req: read_ptr(module_base() + GAME_MAN_RVA).and_then(|gm| read::<u8>(gm + 0x10)),
        anim,
        alive: data.and_then(|d| read::<i32>(d + DATA_HP)).is_some_and(|hp| hp > 0),
        remo: crate::cutscene::raw().map(|r| (r.flags, r.step, r.pending)),
    })
}

fn hex8(b: Option<[u8; 8]>) -> String {
    b.map_or("?".into(), |b| b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" "))
}

fn full(s: &Snap) -> String {
    format!(
        "sp {:?}; chr+1F40 [{}]; +1A10 [{}]; data+228 {:?}; team {:?} type {:?}; menu mask {:?}; menu states {:?}; throw (node, running) {:?}; talk idle {}; paused {:?}; load req {:?}; anim {}; alive {}; remo (flags, step, pending) {:?}",
        s.sp,
        hex8(s.chr_flags),
        hex8(s.b1a10),
        s.data_flags.map(|v| format!("{v:#x}")),
        s.team,
        s.ctype,
        s.menu_mask.map(|v| format!("{v:#x}")),
        s.menu_states,
        s.throw,
        s.talk_idle,
        s.paused,
        s.load_req,
        s.anim,
        s.alive,
        s.remo
    )
}

fn diff(a: &Snap, b: &Snap) -> String {
    let mut parts = Vec::new();
    if a.sp != b.sp {
        let added: Vec<i32> = b.sp.iter().copied().filter(|x| !a.sp.contains(x)).collect();
        let removed: Vec<i32> = a.sp.iter().copied().filter(|x| !b.sp.contains(x)).collect();
        parts.push(format!("sp +{added:?} -{removed:?} (now {})", b.sp.len()));
    }
    if a.chr_flags != b.chr_flags {
        parts.push(format!("chr+1F40 [{}] -> [{}]", hex8(a.chr_flags), hex8(b.chr_flags)));
    }
    if a.b1a10 != b.b1a10 {
        parts.push(format!("+1A10 [{}] -> [{}]", hex8(a.b1a10), hex8(b.b1a10)));
    }
    if a.data_flags != b.data_flags {
        parts.push(format!("data+228 {:?} -> {:?}", a.data_flags.map(|v| format!("{v:#x}")), b.data_flags.map(|v| format!("{v:#x}"))));
    }
    if a.team != b.team || a.ctype != b.ctype {
        parts.push(format!("team/type {:?}/{:?} -> {:?}/{:?}", a.team, a.ctype, b.team, b.ctype));
    }
    if a.menu_mask != b.menu_mask {
        parts.push(format!("menu mask {:?} -> {:?}", a.menu_mask.map(|v| format!("{v:#x}")), b.menu_mask.map(|v| format!("{v:#x}"))));
    }
    if a.menu_states != b.menu_states {
        let added: Vec<_> = b.menu_states.iter().filter(|x| !a.menu_states.contains(x)).collect();
        let removed: Vec<_> = a.menu_states.iter().filter(|x| !b.menu_states.contains(x)).collect();
        parts.push(format!("menu states +{added:?} -{removed:?}"));
    }
    if a.throw != b.throw {
        parts.push(format!("throw (node, running) {:?} -> {:?}", a.throw, b.throw));
    }
    if a.talk_idle != b.talk_idle {
        parts.push(format!("talk idle {} -> {}", a.talk_idle, b.talk_idle));
    }
    if a.paused != b.paused {
        parts.push(format!("paused {:?} -> {:?}", a.paused, b.paused));
    }
    if a.load_req != b.load_req {
        parts.push(format!("load req {:?} -> {:?}", a.load_req, b.load_req));
    }
    if a.remo != b.remo {
        parts.push(format!("remo (flags, step, pending) {:?} -> {:?}", a.remo, b.remo));
    }
    if a.alive != b.alive {
        parts.push(format!("alive {} -> {}", a.alive, b.alive));
    }
    parts.join("; ")
}

fn recent_flags(st: &mut State) -> String {
    let now = game::now_ms();
    while st.flags.front().is_some_and(|f| now.saturating_sub(f.0) > FLAGS_WINDOW_MS) {
        st.flags.pop_front();
    }
    if st.flags.is_empty() {
        return String::new();
    }
    let v: Vec<String> = st.flags.iter().map(|(_, id, on, ours)| format!("{}{id}={}", if *ours { "us:" } else { "fr:" }, *on as u8)).collect();
    format!(" | flags 2 s [{}]", v.join(", "))
}

/// world.rs: a shared flag we sent (`ours`) or the friend's that we applied.
pub fn flag_write(id: u32, on: bool, ours: bool) {
    if !enabled() {
        return;
    }
    if let Ok(mut st) = STATE.try_lock() {
        if st.flags.len() >= 64 {
            st.flags.pop_front();
        }
        st.flags.push_back((game::now_ms(), id, on, ours));
    }
}

/// Per frame (puppet.rs). `anim` = the player's current animation (0 = unknown).
pub fn tick(anim: i32) {
    if !enabled() {
        return;
    }
    let Ok(mut st) = STATE.try_lock() else { return };
    if st.at.is_some_and(|t| t.elapsed() < EVERY) {
        return;
    }
    st.at = Some(Instant::now());
    let Some(s) = snap(anim) else {
        st.last = None;
        return;
    };
    match st.last.take() {
        None => {
            let f = recent_flags(&mut st);
            log::line(&format!("state: player (first sight): {}{f}", full(&s)));
        }
        Some(prev) => {
            // The animation alone changes all the time: it rides along with other changes.
            // v8.15: the Resurrect verdict in plain words (never rate limited). Offered: the
            // SpEffects 110090 (+110010 / 110091 / 110092) come on; not offered: 110093 alone
            // (30+ deaths in the 2026-10-25 logs).
            if s.sp.contains(&110090) && !prev.sp.contains(&110090) {
                log::line("resurrect: OFFERED (SpEffect 110090 on)");
                resurrect_dump("offered");
            } else if s.sp.contains(&110093) && !prev.sp.contains(&110093) && !s.sp.contains(&110090) {
                log::line("resurrect: NOT offered (SpEffect 110093 without 110090)");
                resurrect_dump("not offered");
            }
            let mut a = prev.clone();
            a.anim = s.anim;
            if a != s && st.lines < MAX_CHANGE_LINES {
                st.lines += 1;
                let f = recent_flags(&mut st);
                log::line(&format!("state: {} (anim {}){f}", diff(&prev, &s), s.anim));
            }
        }
    }
    st.last = Some(s);
}

/// v8.20: the Resurrect inputs (lock value, goods 1110 / 1100 / 2000) and every SpEffect on the
/// player, one line. Called at a death, at the OFFERED / NOT offered verdict, on F11 and when
/// the lock value changes.
pub fn resurrect_dump(why: &str) {
    let sp = game::player().map(|p| sp_ids(p.0)).unwrap_or_default();
    let blocks = game::player().map_or_else(|| "no player".to_string(), |p| resurrect_blocks(p.0));
    log::line(&format!("resurrect dump ({why}): {}; SpEffects {sp:?}; {blocks}", crate::invguard::resurrect_fields()));
}

/// SpEffect node: param row pointer; SpEffectParam row: stateInfo (u16).
const SP_NODE_PARAM: usize = 0x60;
const SP_PARAM_STATE_INFO: usize = 0x15E;
/// stateInfo values that keep the Resurrect window shut (`0xA2CC30`, IDA 2026-10-27).
const NO_REVIVE_STATES: [u16; 2] = [0x155, 0x167];
const PGD_NO_REVIVE: usize = 0x178;

/// v8.58: the other inputs of `0xA2CC30`, which opens the Resurrect window at death (chr+0x2138
/// = chr+0x2134) unless `0xA34B70` fails, an active SpEffect has stateInfo 0x155 / 0x167, or the
/// byte PlayerGameData+0x178 is set. Live 2026-10-27: not offered with lock 0 and nodes, the
/// mod not holding the death step (17:33:01, 17:35:35). Diagnostic, read only.
fn resurrect_blocks(p: usize) -> String {
    let mut states = Vec::new();
    if let Some(list) = read_ptr(p + CHR_SP_LIST) {
        let mut node = read_ptr(list + SP_FIRST);
        let mut n = 0;
        while let Some(x) = node {
            n += 1;
            if n > SP_MAX {
                break;
            }
            let st = read_ptr(x + SP_NODE_PARAM).and_then(|r| read::<u16>(r + SP_PARAM_STATE_INFO));
            if let Some(st) = st.filter(|s| NO_REVIVE_STATES.contains(s)) {
                states.push(format!("{} (state {st:#x})", read::<i32>(x + SP_NODE_ID).unwrap_or(-1)));
            }
            node = read_ptr(x + SP_NODE_NEXT).filter(|&y| y != x);
        }
    }
    let pgd = crate::invguard::player_game_data();
    let b178 = pgd.and_then(|g| read::<u8>(g + PGD_NO_REVIVE));
    let win = read::<f32>(p + 0x2134);
    format!("window blocks: SpEffects with state 0x155/0x167 {states:?}, PGD+0x178 {b178:?}, window length chr+0x2134 {win:?}")
}

/// SpEffect node flags (`node+0x1C`): any of these bits = the node does not count (`0xBFEBF0`).
const SP_NODE_FLAGS: usize = 0x1C;
const SP_NODE_INACTIVE: u32 = 0x800C_0003;
/// stateInfo 302: the chr's team for every targeting and hit check (`0x9F1510`) is its owner's
/// team instead of chr+0x74, but only while chr+0x70 == 2 (IDA 2026-10-27, `F:\Temp\re_puppet`).
/// The Puppeteer Ninjutsu puppet [I: the only vanilla use we know that turns an enemy to our side].
const STATE_OWNER_TEAM: u16 = 302;
const OWNER_TEAM_TYPE: u32 = 2;

/// `0x9F1510`'s test: stateInfo 302 active and chr+0x70 == 2.
pub fn takes_owner_team(chr_type: u32, state_on: bool) -> bool {
    state_on && chr_type == OWNER_TEAM_TYPE
}

/// The first active SpEffect id on `chr` with this stateInfo (`0xBFEBF0` without its bitmask
/// pre-check, which only skips the walk).
fn active_state(chr: usize, state: u16) -> Option<i32> {
    let list = read_ptr(chr + CHR_SP_LIST)?;
    let mut node = read_ptr(list + SP_FIRST);
    let mut n = 0;
    while let Some(x) = node {
        n += 1;
        if n > SP_MAX {
            break;
        }
        let on = read::<u32>(x + SP_NODE_FLAGS).is_some_and(|f| f & SP_NODE_INACTIVE == 0);
        if on && read_ptr(x + SP_NODE_PARAM).and_then(|r| read::<u16>(r + SP_PARAM_STATE_INFO)) == Some(state) {
            return read::<i32>(x + SP_NODE_ID);
        }
        node = read_ptr(x + SP_NODE_NEXT).filter(|&y| y != x);
    }
    None
}

/// livesync.rs / persist.rs: an enemy the game has put on its owner's side (a Puppeteer
/// puppet): `Some((SpEffect id, takes the owner's team))`. `(id, false)` = stateInfo 302 on but
/// chr+0x70 != 2, so the game keeps its own team (logged, not treated as a puppet).
pub(crate) fn owner_team_state(chr: usize) -> Option<(i32, bool)> {
    let id = active_state(chr, STATE_OWNER_TEAM)?;
    let ty = read::<u32>(chr + CHR_TYPE).unwrap_or(u32::MAX);
    Some((id, takes_owner_team(ty, true)))
}

/// F11: the full state once.
pub fn dump(anim: i32) {
    let Ok(mut st) = STATE.lock() else { return };
    let f = recent_flags(&mut st);
    match snap(anim) {
        Some(s) => log::line(&format!("state: F11 FULL: {}{f}", full(&s))),
        None => log::line("state: F11 FULL: no player"),
    }
    drop(st);
    resurrect_dump("F11");
    scale_dump();
}

/// `scale: F11` lines: scaling facts and posture of the nearest enemies and the locked one.
fn scale_dump() {
    let enemies: Vec<(i32, game::Chr)> = crate::nsession::enemy_chrs().into_iter().map(|c| (c.entity_id().unwrap_or(-1), c)).collect();
    let ppos = game::player().and_then(|p| p.position());
    let tag = |c: game::Chr| match crate::netai::net_id_of(c) {
        Some(n) => format!(
            " net={n} owned_here={} forced={} game={}",
            crate::nsession::owned_here(n),
            crate::nsession::forced_here(n),
            crate::nsession::game_owned_here(n)
        ),
        None => " net=none".to_string(),
    };
    for l in crate::scaling::snapshot_lines(enemies, ppos, crate::spectate_cam::lock_mark(), tag).into_iter().take(20) {
        log::line(&l);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_team_needs_the_state_and_chr_type_2() {
        assert!(takes_owner_team(2, true));
        assert!(!takes_owner_team(0, true));
        assert!(!takes_owner_team(2, false));
    }

    #[test]
    fn resurrect_verdict_reads_sp_effects() {
        assert_eq!(resurrect_verdict(&[]), None);
        assert_eq!(resurrect_verdict(&[110010, 110090, 110091]), Some(true));
        assert_eq!(resurrect_verdict(&[110093]), Some(false));
        assert_eq!(resurrect_verdict(&[110090, 110093]), Some(true));
    }

    #[test]
    fn nonzero_states_reads_ids_past_255() {
        let mut raw = vec![0u8; 4 * MENU_STATE_COUNT];
        raw[4 * 102..4 * 102 + 4].copy_from_slice(&1i32.to_le_bytes());
        raw[4 * 300..4 * 300 + 4].copy_from_slice(&(-1i32).to_le_bytes());
        raw[4 * 999..4 * 999 + 4].copy_from_slice(&7i32.to_le_bytes());
        assert_eq!(nonzero_states(&raw), vec![(102, 1), (300, -1), (999, 7)]);
    }

    #[test]
    fn nonzero_states_empty_and_partial() {
        assert!(nonzero_states(&[0u8; 16]).is_empty());
        // A trailing partial word is ignored, not read past.
        assert_eq!(nonzero_states(&[2, 0, 0, 0, 9]), vec![(0, 2)]);
    }

    #[test]
    fn diff_reports_remo_change() {
        let a = Snap { remo: Some((1, 4, 0)), ..Default::default() };
        let b = Snap { remo: Some((0, -1, 0)), ..Default::default() };
        assert!(diff(&a, &b).contains("remo (flags, step, pending) Some((1, 4, 0)) -> Some((0, -1, 0))"));
    }

    #[test]
    fn diff_reports_throw_change() {
        let a = Snap { throw: Some((false, false)), ..Default::default() };
        let b = Snap { throw: Some((true, true)), ..Default::default() };
        assert!(diff(&a, &b).contains("throw (node, running) Some((false, false)) -> Some((true, true))"));
        assert!(diff(&a, &a).is_empty());
    }
}
