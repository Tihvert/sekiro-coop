//! Posture and deathblow sync next to the HP sync (setting `sync_posture`,
//! IMPLEMENTATION-NOTES § 20, protocol VERSION 9, NOT verified live).
//!
//! Game side [V, IDA on a private re7db copy, F:\Temp\ida_post o1-o6]:
//! - Posture lives in the chr data module (`*(*(chr+0x1FF8)+0x18)`): current
//!   `+0x148`, max `+0x14C` (base max `+0x150`, scaling.rs). It counts DOWN:
//!   the hit path `0xB6E6A0` calls `0xBD4DE0(data, -hit.postureDamage(+484), 1)`.
//!   `0xBD4DE0(data, delta, smooth)` = `0xBD6710(data, cur + delta, smooth)`,
//!   which clamps through `0xBD4E20` (NpcParam minimum .. max), keeps the
//!   lagging bar `+0x258` (smooth = 1 lets it fall gradually), refuses a
//!   decrease while `+0x228 & 0x10` (posture-invincible) is set, and notifies
//!   `0xBD6200`. Recovery also goes through `0xBD6710` (callers `0xBD60F0`,
//!   `0xBD62D0`, `0xBD6AE0`, `0xBD6DC0`), always as an increase.
//! - The break is a hit reaction, not a per-frame check: `0xB6E6A0` sets the
//!   hit's break byte (`+0x1C6`) when the posture after the hit would be <= 0,
//!   and `0xB6BD40` turns it into damage-reaction 1027. So posture we lower
//!   out-of-band leaves the enemy at 0 posture without the stagger; its NEXT
//!   hit on the host breaks it (and opens the deathblow) the vanilla way.
//! - Lives: `data+0x25C` (i32), initialised from NpcParam `+0x126` (the
//!   deathblow count, scaling.rs `NPC_DEATHBLOWS`) by `0xBD6AE0`, reset to it
//!   by the revive `0x9F4360`. `0xBD5DF0` ("cannot die now") is true while
//!   it is > 0, and the HP setter `0xBD64E0(data, hp)` then keeps HP at 1
//!   instead of 0. A deathblow is a hit of type 5 (`hit+0x28`): `0xB6E6A0`
//!   decrements `+0x25C` inline and plays the life SFX `0x9F0410(chr)`
//!   (NpcParam `+852` / `+856`) before the HP damage. No standalone function
//!   exists for it, so the host does the same two steps here.
//!
//! Client: frame-to-frame, a posture drop of an enemy we track (not caused by
//! our own write, not a max change) is our hit, a lives drop is our
//! deathblow (the native remote's hits on the client copy are reaction-only,
//! artfx.rs). Both are kept as session totals and sent with the HP total in
//! one `DamageReport`. The host's posture (and lives) come back in `Posture`
//! states and are written with `0xBD4DE0` only when they differ by more than
//! 5 % of max, never while our own hits are still on their way, never while
//! the enemy is broken here.
//! Host: applies the increases with the game's functions. A deathblow that
//! leaves the enemy a life refills HP and posture (the enemy gets up for its
//! next phase) and the same report's HP loss is dropped.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use coop_net::{DamageReport, PostureState};

use crate::game::{self, Chr};
use crate::log;
use crate::net::Net;
use crate::peers::PeerId;
use crate::tuning;

/// `[GAMEPLAY] posture_rest_guard` (v7.7): no host-lives mirroring in the 13 s rest
/// window. 0 = v7.6 (always mirror). Default 1.
pub static REST_GUARD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// No host lives onto our copies this long after a death reload of ours (v8.6, persist.rs
/// `after_death_guard`): covers the other player's later reload in a wipe.
const DEATH_LIVES_GUARD: std::time::Duration = std::time::Duration::from_secs(45);
const CHR_MODULES: usize = 0x1FF8;
const MOD_DATA: usize = 0x18;
const DATA_HP: usize = 0x130;
const DATA_MAX_HP: usize = 0x134;
const DATA_POSTURE: usize = 0x148;
const DATA_MAX_POSTURE: usize = 0x14C;
const DATA_LIVES: usize = 0x25C;

/// `void (data, int delta, bool smooth_bar)`: posture += delta, game clamps.
const ADD_POSTURE_RVA: usize = 0xBD4DE0;
/// `void (data, int hp)`: HP setter with the max clamp and the "cannot die" floor.
const SET_HP_RVA: usize = 0xBD64E0;
/// `void (ChrIns*)`: the life-consumed SFX of a deathblow.
const LIFE_SFX_RVA: usize = 0x9F0410;

const HOST_SEND_EVERY: Duration = Duration::from_millis(100);
/// Host: an enemy back at full posture is still sent this long (the client
/// sees the change).
const HOST_SEND_AFTER_CHANGE: Duration = Duration::from_millis(1500);
/// Client: a host state older than this is not mirrored.
const HOST_FRESH: Duration = Duration::from_millis(1000);
/// Client: our reports are re-sent this long after the last posture change.
const RESEND_FOR: Duration = Duration::from_secs(3);
/// Client: after our own posture hit the host's (higher) value is not written
/// back for this long (same as enemies.rs OWN_COMBAT_FOR).
const HOLD_FOR: Duration = Duration::from_millis(2500);
/// Client: watch for the local game's own phase refill after our deathblow.
const REFILL_WATCH: Duration = Duration::from_secs(10);
const STATS_EVERY: Duration = Duration::from_secs(10);

/// Posture sync is on: the setting, and the experimental native session does
/// not own the enemies (nsession.rs `step_aside`: stage >= 2 runs the game's
/// own damage sync; enemies.rs already returns early then, this is the
/// second guard).
pub fn enabled() -> bool {
    tuning::sync_posture() && !crate::nsession::step_aside::enemy_sync_off()
}

// --- Pure accounting (unit tested) -----------------------------------------

/// One enemy's posture values as read in one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub chr: usize,
    pub posture: i32,
    pub max: i32,
    pub lives: i32,
}

/// The client's own change between two frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Own {
    /// Posture our hits took off (local units).
    pub posture: i32,
    pub deathblows: i32,
}

/// Our own posture damage and deathblows from `prev` (last frame, after any
/// of our writes) to `now`. Nothing counts for a new / respawned character, a
/// changed max (the game clamps the current value to it: not a hit), or
/// while `counting` is off (enemy not seen alive here for a while).
pub fn own_change(prev: Option<Seen>, now: Seen, counting: bool) -> Own {
    let Some(p) = prev else { return Own::default() };
    if !counting || p.chr != now.chr || p.max != now.max || now.max <= 0 {
        return Own::default();
    }
    Own { posture: (p.posture - now.posture).max(0), deathblows: (p.lives - now.lives).max(0) }
}

/// Posture points from `from_max` units to `to_max` units (both machines
/// normally have the same max: scaling.rs uses the host's percentages).
pub fn convert(v: i32, from_max: i32, to_max: i32) -> i32 {
    if from_max <= 0 || to_max <= 0 || from_max == to_max {
        return v;
    }
    let n = v as i64 * to_max as i64;
    let d = from_max as i64;
    // Rounded to nearest, symmetric for negative values.
    let r = if n >= 0 { (n + d / 2) / d } else { (n - d / 2) / d };
    r.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// Smallest posture difference the client mirror corrects.
pub fn mirror_threshold(max: i32) -> i32 {
    (max / 20).max(5)
}

/// Client: write the host's posture `host` (local units) over `local`?
/// Not while broken here (the local game runs the stagger and the deathblow
/// window), not upwards while our own hits are still on their way, and not
/// for differences the two recoveries make anyway.
pub fn mirror_posture(local: i32, host: i32, max: i32, holding: bool) -> bool {
    if max <= 0 || local <= 0 {
        return false;
    }
    if holding && host > local {
        return false;
    }
    (local - host).abs() >= mirror_threshold(max)
}

/// Client: the host has fewer lives (its own deathblow): copy them. Never
/// upwards (a higher host value is our deathblow not applied yet).
pub fn mirror_lives(local: i32, host: i32) -> bool {
    host >= 0 && host < local
}

/// Host: what the client dealt beyond what was applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub posture: i32,
    pub deathblows: i32,
}

impl Applied {
    /// The increase of each cumulative field (never negative) and records it.
    pub fn take(&mut self, r: &DamageReport) -> (i32, i32) {
        let dp = (r.posture - self.posture).max(0);
        let db = (r.deathblows as i32 - self.deathblows).max(0);
        self.posture = self.posture.max(r.posture);
        self.deathblows = self.deathblows.max(r.deathblows as i32);
        (dp, db)
    }
}

/// Host: what to do with one report's new posture and deathblows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostPlan {
    /// Lives to take (at most the lives the enemy has).
    pub consume: i32,
    /// A life is left after them: back to full HP and posture.
    pub refill: bool,
    /// Apply this report's HP loss (false after a refill: the deathblow's
    /// damage is what the refill replaces).
    pub apply_hp: bool,
    /// Posture to take off.
    pub posture: i32,
}

pub fn host_plan(lives: i32, new_deathblows: i32, new_posture: i32) -> HostPlan {
    let consume = new_deathblows.clamp(0, lives.max(0));
    let refill = consume > 0 && lives - consume > 0;
    HostPlan { consume, refill, apply_hp: !refill, posture: if refill { 0 } else { new_posture.max(0) } }
}

// --- Game access -------------------------------------------------------------

pub(crate) fn data_of(chr: Chr) -> Option<usize> {
    game::read_ptr(chr.0 + CHR_MODULES).and_then(|m| game::read_ptr(m + MOD_DATA))
}

fn read_seen(chr: Chr, data: usize) -> Option<Seen> {
    Some(Seen {
        chr: chr.0,
        posture: game::read::<i32>(data + DATA_POSTURE)?,
        max: game::read::<i32>(data + DATA_MAX_POSTURE)?,
        lives: game::read::<i32>(data + DATA_LIVES)?,
    })
}

pub(crate) fn add_posture(data: usize, delta: i32, smooth: bool) {
    if delta == 0 {
        return;
    }
    let f: extern "C" fn(usize, i32, u8) = unsafe { std::mem::transmute(game::module_base() + ADD_POSTURE_RVA) };
    f(data, delta, smooth as u8);
}

/// The life-consumed SFX of a deathblow (`0x9F0410(chr)`), as the hit path plays it.
pub(crate) fn life_sfx(chr: Chr) {
    let sfx: extern "C" fn(usize) = unsafe { std::mem::transmute(game::module_base() + LIFE_SFX_RVA) };
    sfx(chr.0);
}

/// Host: HP through the game's own setter (`0xBD64E0`): clamped to max, and
/// kept at 1 while the enemy cannot die (lives left, scripted). A raw write
/// of 0 skipped that and killed a multi-life enemy without its deathblow.
pub fn set_hp(chr: Chr, hp: i32) -> bool {
    let Some(data) = data_of(chr) else { return false };
    if game::read::<i32>(data + DATA_HP).is_none() {
        return false;
    }
    let f: extern "C" fn(usize, i32) = unsafe { std::mem::transmute(game::module_base() + SET_HP_RVA) };
    f(data, hp);
    true
}

// --- State -------------------------------------------------------------------

#[derive(Default)]
struct ClientEnemy {
    prev: Option<Seen>,
    /// Session totals (host units / count).
    dealt: i32,
    deathblows: i32,
    last_change: Option<Instant>,
    /// After our deathblow: (when, local HP then), to log a local refill once.
    refill_watch: Option<(Instant, i32)>,
}

struct HostSeen {
    lives0: i32,
    last: (i32, i32),
    changed_at: Instant,
}

#[derive(Default)]
struct Stats {
    // Host.
    friend_hits: u32,
    friend_sum: i64,
    friend_deathblows: u32,
    refills: u32,
    states_sent: u32,
    // Client.
    own_hits: u32,
    own_sum: i64,
    own_deathblows: u32,
    states_rx: u32,
    mirror_writes: u32,
    holds: u32,
    lives_writes: u32,
}

#[derive(Default)]
pub struct Posture {
    // Client.
    client: HashMap<i32, ClientEnemy>,
    host_states: HashMap<i32, (Instant, PostureState)>,
    // Host: per (reporting peer, enemy); each client counts its own totals from 0.
    applied: HashMap<(PeerId, i32), Applied>,
    host_seen: HashMap<i32, HostSeen>,
    last_send: Option<Instant>,
    stats: Stats,
    stats_at: Option<Instant>,
    logs: u32,
    max_logs: u32,
}

impl Posture {
    /// Area load: every chr pointer may be gone. Totals stay (per session,
    /// like enemies.rs `dealt_total` / `applied_damage`).
    pub fn after_load(mut self) -> Self {
        for c in self.client.values_mut() {
            c.prev = None;
            c.refill_watch = None;
        }
        self.host_states.clear();
        self.host_seen.clear();
        self
    }

    /// New session (epoch): both sides restart their totals.
    pub fn new_session(&mut self) {
        self.client.clear();
        self.applied.clear();
        self.host_states.clear();
        self.host_seen.clear();
    }

    /// Host: a peer on a new link (another client stays): only its totals restart.
    pub fn forget_peer(&mut self, from: PeerId) {
        self.applied.retain(|(p, _), _| *p != from);
    }

    fn log_n(&mut self, msg: String) {
        self.logs += 1;
        if self.logs <= 30 || self.logs % 50 == 0 {
            log::line(&msg);
        }
    }

    // --- Client ---

    /// The client's report for `id`: the HP total plus our posture totals.
    pub fn report_hp(&self, id: i32, hp: i32) -> DamageReport {
        let c = self.client.get(&id);
        DamageReport {
            id,
            hp,
            posture: c.map_or(0, |c| c.dealt),
            deathblows: c.map_or(0, |c| c.deathblows.clamp(0, u16::MAX as i32) as u16),
        }
    }

    /// Same, None when there is nothing to report.
    pub fn report(&self, id: i32, hp: Option<i32>) -> Option<DamageReport> {
        let has = self.client.get(&id).is_some_and(|c| c.dealt > 0 || c.deathblows > 0);
        (has || hp.is_some()).then(|| self.report_hp(id, hp.unwrap_or(0)))
    }

    /// Client: ids whose posture totals changed recently (re-sent like HP).
    pub fn recent(&self, now: Instant) -> Vec<i32> {
        self.client
            .iter()
            .filter(|(_, c)| c.last_change.is_some_and(|t| now.duration_since(t) < RESEND_FOR))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Client: host posture states of this frame.
    pub fn receive(&mut self, list: Vec<coop_net::PostureState>, now: Instant) {
        self.stats.states_rx += list.len() as u32;
        for s in list {
            self.host_states.insert(s.id, (now, s));
        }
        if self.host_states.len() > 256 {
            self.host_states.retain(|_, (t, _)| now.duration_since(*t) < HOST_FRESH);
        }
        self.stats_tick(now, false);
    }

    /// Client, per mirrored enemy and frame. `counting`: seen alive here long
    /// enough for its changes to be ours (enemies.rs `alive_before`);
    /// `own_combat`: our HP hit landed recently. True = our totals changed,
    /// send the report now.
    pub fn client_step(&mut self, id: i32, chr: Chr, counting: bool, own_combat: bool, now: Instant) -> bool {
        let Some(data) = data_of(chr) else { return false };
        let Some(cur) = read_seen(chr, data) else { return false };
        let host = self.host_states.get(&id).filter(|(t, _)| now.duration_since(*t) < HOST_FRESH).map(|(_, s)| *s);
        let mut msgs = Vec::new();
        let st = self.client.entry(id).or_default();
        // v8.15: nothing counts as ours inside the death-reload guard. persist.rs restores the
        // boss record (lives 2 -> 1) onto the fresh copy there, and that drop was counted as our
        // own deathblow and sent to the host (live 2026-10-25: 1700201 "friend deathblow lives
        // 1 -> 0" while the friend stood at the idol 75 m away).
        let own = if crate::persist::after_death_guard(DEATH_LIVES_GUARD) {
            Own::default()
        } else {
            own_change(st.prev, cur, counting)
        };
        let mut changed = false;
        if own.posture > 0 {
            let d = host.map_or(own.posture, |h| convert(own.posture, cur.max, h.max));
            st.dealt = st.dealt.saturating_add(d);
            st.last_change = Some(now);
            changed = true;
            self.stats.own_hits += 1;
            self.stats.own_sum += d as i64;
            msgs.push(format!("posture: own hit {id} +{d} (here {}/{})", cur.posture, cur.max));
        }
        if own.deathblows > 0 {
            st.deathblows += own.deathblows;
            st.last_change = Some(now);
            changed = true;
            st.refill_watch = game::read::<i32>(data + DATA_HP).map(|hp| (now, hp));
            self.stats.own_deathblows += own.deathblows as u32;
            // Always logged (rare, and the live check for item 4).
            log::line(&format!(
                "posture: own deathblow {id}: lives {} -> {} (total {})",
                st.prev.map_or(-1, |p| p.lives),
                cur.lives,
                st.deathblows
            ));
        }
        // The local game's phase refill after our deathblow (live check: does
        // the client copy refill by itself? the host refills in host_apply).
        if let Some((t, hp0)) = st.refill_watch {
            let hp = game::read::<i32>(data + DATA_HP).unwrap_or(hp0);
            let max_hp = game::read::<i32>(data + DATA_MAX_HP).unwrap_or(0);
            if now.duration_since(t) > REFILL_WATCH {
                st.refill_watch = None;
            } else if max_hp > 0 && hp > hp0 + max_hp / 10 {
                st.refill_watch = None;
                log::line(&format!("posture: {id} hp refilled here after our deathblow ({hp0} -> {hp}/{max_hp})"));
            }
        }

        let mut after = cur;
        if let Some(h) = host {
            // Not in the rest window (restsync.rs): the host's lives may be from before its
            // respawn of the same rest (live 2026-10-24: 1120300 lost a life to the host's
            // pre-rest state, then livesync lowered the host's fresh copy from ours, §35).
            // Nor right after a death reload of ours (a wipe: the host may still be in the
            // old world, persist.rs `after_death_guard`); after a plain death persist's
            // boss restore brings the host's lives instead.
            if !(REST_GUARD.load(std::sync::atomic::Ordering::Relaxed) && crate::restsync::hold_kills())
                && !crate::persist::after_death_guard(DEATH_LIVES_GUARD)
                && !crate::persist::wipe_recent(crate::persist::WIPE_STALE)
                && mirror_lives(cur.lives, h.lives as i32)
                && game::write(data + DATA_LIVES, h.lives as i32)
            {
                after.lives = h.lives as i32;
                self.stats.lives_writes += 1;
                msgs.push(format!("posture: {id} lives {} -> {} (host)", cur.lives, h.lives));
            }
            if h.max != cur.max && self.max_logs < 10 {
                self.max_logs += 1;
                msgs.push(format!("posture: {id} max host {} here {} (values converted)", h.max, cur.max));
            }
            let target = convert(h.posture, h.max, cur.max);
            let holding = own_combat || st.last_change.is_some_and(|t| now.duration_since(t) < HOLD_FOR);
            if mirror_posture(cur.posture, target, cur.max, holding) {
                add_posture(data, target - cur.posture, false);
                after.posture = game::read::<i32>(data + DATA_POSTURE).unwrap_or(cur.posture);
                self.stats.mirror_writes += 1;
            } else if holding && target > cur.posture + mirror_threshold(cur.max) {
                self.stats.holds += 1;
            }
        }
        st.prev = Some(after);
        for m in msgs {
            self.log_n(m);
        }
        changed
    }

    // --- Host ---

    /// Host: one client report's posture and deathblows, before its HP. False
    /// = drop this report's HP loss (a deathblow refilled the enemy). `from`: the
    /// reporting peer (its totals are its own).
    pub fn host_apply(&mut self, from: PeerId, chr: Option<Chr>, r: &DamageReport) -> bool {
        let (dp, db) = self.applied.entry((from, r.id)).or_default().take(r);
        if dp == 0 && db == 0 {
            return true;
        }
        let Some(chr) = chr else { return true };
        let Some(data) = data_of(chr) else { return true };
        let Some(s) = read_seen(chr, data) else { return true };
        if game::read::<i32>(data + DATA_HP).is_none_or(|hp| hp <= 0) {
            return true;
        }
        let id = r.id;
        let plan = host_plan(s.lives, db, dp);
        if db > 0 {
            self.stats.friend_deathblows += db as u32;
        }
        if plan.consume > 0 {
            let left = s.lives - plan.consume;
            game::write(data + DATA_LIVES, left);
            let sfx: extern "C" fn(usize) = unsafe { std::mem::transmute(game::module_base() + LIFE_SFX_RVA) };
            sfx(chr.0);
            log::line(&format!("posture: friend {} deathblow {id}: lives {} -> {left}", from.slot(), s.lives));
        } else if db > 0 {
            log::line(&format!("posture: friend {} deathblow {id} with no life left here (lives {}): HP decides", from.slot(), s.lives));
        }
        if plan.refill {
            let max_hp = game::read::<i32>(data + DATA_MAX_HP).unwrap_or(0);
            if max_hp > 0 {
                set_hp(chr, max_hp);
            }
            add_posture(data, s.max.max(0), false);
            self.stats.refills += 1;
            log::line(&format!("posture: {id} gets up for its next life: hp {max_hp}, posture full"));
        }
        if plan.posture > 0 {
            add_posture(data, -plan.posture, true);
            let now_p = game::read::<i32>(data + DATA_POSTURE).unwrap_or(s.posture);
            self.stats.friend_hits += 1;
            self.stats.friend_sum += plan.posture as i64;
            self.log_n(format!("posture: friend {} hit {id} +{} (now {now_p}/{})", from.slot(), plan.posture, s.max));
        }
        plan.apply_hp
    }

    /// Host, every frame: posture states of the synced enemies (10 Hz), only
    /// those not at full posture, short of lives, or just changed.
    pub fn host_send(&mut self, net: &mut Net, enemies: &[(i32, Chr)], now: Instant) {
        self.stats_tick(now, true);
        if self.last_send.is_some_and(|t| now.duration_since(t) < HOST_SEND_EVERY) {
            return;
        }
        self.last_send = Some(now);
        let mut list = Vec::new();
        for &(id, chr) in enemies {
            let Some(data) = data_of(chr) else { continue };
            let Some(s) = read_seen(chr, data) else { continue };
            if s.max <= 0 || game::read::<i32>(data + DATA_HP).is_none_or(|hp| hp <= 0) {
                continue;
            }
            let e = self.host_seen.entry(id).or_insert(HostSeen { lives0: s.lives, last: (s.posture, s.lives), changed_at: now });
            if (s.posture, s.lives) != e.last {
                e.last = (s.posture, s.lives);
                e.changed_at = now;
            }
            e.lives0 = e.lives0.max(s.lives);
            if s.posture < s.max || s.lives < e.lives0 || now.duration_since(e.changed_at) < HOST_SEND_AFTER_CHANGE {
                list.push(PostureState { id, posture: s.posture, max: s.max, lives: s.lives.clamp(-128, 127) as i8 });
            }
        }
        if !list.is_empty() {
            self.stats.states_sent += list.len() as u32;
            net.send_posture(&list);
        }
        if self.host_seen.len() > 512 {
            self.host_seen.retain(|id, _| enemies.iter().any(|(e, _)| e == id));
        }
    }

    fn stats_tick(&mut self, now: Instant, is_host: bool) {
        let Some(at) = self.stats_at else {
            self.stats_at = Some(now);
            return;
        };
        if now.duration_since(at) < STATS_EVERY {
            return;
        }
        self.stats_at = Some(now);
        let s = std::mem::take(&mut self.stats);
        if is_host {
            log::line(&format!(
                "posture: host 10 s: friend hits {} (+{}), deathblows {}, refills {}, states sent {}",
                s.friend_hits, s.friend_sum, s.friend_deathblows, s.refills, s.states_sent
            ));
        } else {
            log::line(&format!(
                "posture: client 10 s: own hits {} (+{}), deathblows {}, host states {}, mirror writes {}, held {}, lives copied {}",
                s.own_hits, s.own_sum, s.own_deathblows, s.states_rx, s.mirror_writes, s.holds, s.lives_writes
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(posture: i32, lives: i32) -> Seen {
        Seen { chr: 0x1000, posture, max: 1000, lives }
    }

    #[test]
    fn own_change_counts_only_drops_of_a_known_character() {
        // First sight, or not counting: nothing.
        assert_eq!(own_change(None, seen(500, 1), true), Own::default());
        assert_eq!(own_change(Some(seen(900, 1)), seen(500, 1), false), Own::default());
        // A drop is our hit; recovery (a rise) is not.
        assert_eq!(own_change(Some(seen(900, 1)), seen(760, 1), true), Own { posture: 140, deathblows: 0 });
        assert_eq!(own_change(Some(seen(760, 1)), seen(780, 1), true), Own::default());
        // A lives drop is our deathblow, a refill on the same frame is not a hit.
        assert_eq!(own_change(Some(seen(0, 2)), seen(1000, 1), true), Own { posture: 0, deathblows: 1 });
        // Respawn (new pointer) or max change (game clamp): not ours.
        let mut other = seen(100, 1);
        other.chr = 0x2000;
        assert_eq!(own_change(Some(seen(900, 1)), other, true), Own::default());
        let mut lower_max = seen(800, 1);
        lower_max.max = 800;
        assert_eq!(own_change(Some(seen(1000, 1)), lower_max, true), Own::default());
    }

    #[test]
    fn mirror_write_then_rebase_is_not_counted() {
        // Frame 1: our hit 900 -> 700. Frame 2: the host's 650 is written
        // (and becomes `prev`), frame 3 reads it back: no second count.
        let a = own_change(Some(seen(900, 1)), seen(700, 1), true);
        assert_eq!(a.posture, 200);
        let after_write = seen(650, 1);
        assert_eq!(own_change(Some(after_write), seen(650, 1), true), Own::default());
    }

    #[test]
    fn totals_are_idempotent_on_the_host() {
        let mut a = Applied::default();
        let r = |posture, deathblows| DamageReport { id: 1, hp: 0, posture, deathblows };
        assert_eq!(a.take(&r(120, 0)), (120, 0));
        // The same report again (resend), and an older one (reordered): nothing.
        assert_eq!(a.take(&r(120, 0)), (0, 0));
        assert_eq!(a.take(&r(80, 0)), (0, 0));
        assert_eq!(a.take(&r(300, 1)), (180, 1));
        assert_eq!(a.take(&r(300, 1)), (0, 0));
        assert_eq!(a, Applied { posture: 300, deathblows: 1 });
    }

    #[test]
    fn deathblow_consumes_exactly_the_lives_reported() {
        // Two-life boss, our first deathblow: one life, refill, HP loss dropped.
        assert_eq!(host_plan(2, 1, 400), HostPlan { consume: 1, refill: true, apply_hp: false, posture: 0 });
        // Its last life: no refill, the HP loss kills it.
        assert_eq!(host_plan(1, 1, 0), HostPlan { consume: 1, refill: false, apply_hp: true, posture: 0 });
        // The host already used that life up (its own deathblow): HP decides.
        assert_eq!(host_plan(0, 1, 50), HostPlan { consume: 0, refill: false, apply_hp: true, posture: 50 });
        // Two reports merged into one (lost packets): never more than it has.
        assert_eq!(host_plan(1, 3, 0).consume, 1);
        assert_eq!(host_plan(3, 2, 0), HostPlan { consume: 2, refill: true, apply_hp: false, posture: 0 });
        // Plain posture hit.
        assert_eq!(host_plan(2, 0, 75), HostPlan { consume: 0, refill: false, apply_hp: true, posture: 75 });
    }

    #[test]
    fn units_convert_between_different_max_values() {
        assert_eq!(convert(150, 1000, 1000), 150);
        assert_eq!(convert(150, 1000, 1500), 225);
        assert_eq!(convert(-100, 1500, 1000), -67);
        assert_eq!(convert(1, 3, 2), 1);
        assert_eq!(convert(42, 0, 1000), 42);
        assert_eq!(convert(i32::MAX, 1, 2), i32::MAX);
    }

    #[test]
    fn mirror_rules() {
        // Small drift from the two recoveries: left alone.
        assert!(!mirror_posture(900, 880, 1000, false));
        // Host's hit (lower) or the host recovered after a break: written.
        assert!(mirror_posture(900, 700, 1000, false));
        assert!(mirror_posture(300, 1000, 1000, false));
        // Our hit is not on the host yet: never raised back, but lowered.
        assert!(!mirror_posture(300, 900, 1000, true));
        assert!(mirror_posture(900, 300, 1000, true));
        // Broken here: the local game owns the stagger.
        assert!(!mirror_posture(0, 1000, 1000, false));
        assert!(!mirror_posture(-5, 1000, 1000, false));
        assert_eq!(mirror_threshold(60), 5);
        assert!(mirror_lives(2, 1) && !mirror_lives(1, 2) && !mirror_lives(1, 1) && !mirror_lives(1, -1));
    }

    #[test]
    fn report_carries_totals_and_skips_empty() {
        let mut p = Posture::default();
        assert_eq!(p.report(5, None), None);
        assert_eq!(p.report(5, Some(30)), Some(DamageReport { id: 5, hp: 30, posture: 0, deathblows: 0 }));
        p.client.insert(5, ClientEnemy { dealt: 210, deathblows: 1, ..Default::default() });
        assert_eq!(p.report(5, None), Some(DamageReport { id: 5, hp: 0, posture: 210, deathblows: 1 }));
        // A load keeps totals, a new session clears them.
        let mut p = p.after_load();
        assert_eq!(p.report_hp(5, 7).posture, 210);
        p.new_session();
        assert_eq!(p.report(5, None), None);
    }

    #[test]
    fn host_totals_are_per_peer_and_enemy() {
        // Two clients' cumulative totals on one enemy are separate (no chr: only the
        // accounting runs).
        let mut p = Posture::default();
        let r = |posture, deathblows| DamageReport { id: 9, hp: 0, posture, deathblows };
        let (a, b) = (PeerId(1), PeerId(2));
        assert!(p.host_apply(a, None, &r(100, 0)));
        assert!(p.host_apply(b, None, &r(60, 1)));
        assert_eq!(p.applied[&(a, 9)], Applied { posture: 100, deathblows: 0 });
        assert_eq!(p.applied[&(b, 9)], Applied { posture: 60, deathblows: 1 });
        // B's repeat adds nothing; A's next total only its increase.
        assert!(p.host_apply(b, None, &r(60, 1)));
        assert_eq!(p.applied[&(b, 9)], Applied { posture: 60, deathblows: 1 });
        assert_eq!(p.applied.get_mut(&(a, 9)).unwrap().take(&r(130, 0)), (30, 0));
        // A rejoins on a new link: only A's totals restart.
        p.forget_peer(a);
        assert!(!p.applied.contains_key(&(a, 9)));
        assert!(p.applied.contains_key(&(b, 9)));
        p.new_session();
        assert!(p.applied.is_empty());
    }
}
