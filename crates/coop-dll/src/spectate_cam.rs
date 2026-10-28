//! Spectator camera (docs/SPEC-BOSS-RULES.md §1.3): while we lie dead in a boss
//! fight and wait for the friend, the rendered camera follows the friend.
//!
//! It uses the game's own free-cam path. `0x83B150` (GameRend::Update, called by
//! the field update right after the ChrCam update) copies the DebugCam
//! `*(GameRend+0xE8)` into the render camera `*(GameRend+0x18)` and pushes it to
//! the SprjCamera singleton (`0xB22770(*(base+0x3D85008), cam)`) whenever
//! `*(i32*)(GameRend+0xE0) != 0` (IDA, 0x83B3FC). The entry hook here writes our
//! pose into the DebugCam right before that copy and switches `+0xE0` on, and
//! later back to the value it had (the DebugCam contents are restored too).
//!
//! The frame task only computes the pose (`SpectateCam::update`); every write
//! to GameRend happens in the hook, on the thread that is updating that very
//! GameRend, except `restore_now`, which re-reads the pointer chain first.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::game::{self, Chr, module_base, read, read_ptr, write};
use crate::hook;
use crate::log;
use crate::peers::{MAX_PLAYERS, PeerFrame, PeerId, PeerSet};
use crate::spectate_input::{self as input, wrap_pi};
use crate::tuning;

const REND_UPDATE_RVA: usize = 0x83B150;
// mov rax, rsp; push rbp; push rdi (no RIP-relative code)
const REND_UPDATE_PROLOGUE: [u8; 5] = [0x48, 0x8B, 0xC4, 0x55, 0x57];
/// `GameRend = *(*(base+0x3D5C0A0) + 0x20)` (RE-FINDINGS-2 §3, also used by ui.rs).
const FIELD_AREA_RVA: usize = 0x3D5C0A0;
const FIELD_GAME_REND: usize = 0x20;
/// Render camera (SprjPersCam*) handed to the renderer.
const REND_CAM: usize = 0x18;
/// Free-cam switch (i32): non-zero = DebugCam replaces the render camera.
const REND_FREE_CAM: usize = 0xE0;
/// DebugCam (SprjPersCam*), allocated by the GameRend ctor `0x839CC0`.
const REND_DEBUG_CAM: usize = 0xE8;
/// SprjPersCam: rows right / up / forward / position (float4 each), then fov,
/// aspect, near, far (f32).
const CAM_ROWS: usize = 0x10;
const CAM_FOV: usize = 0x50;
/// The DebugCam bytes we overwrite and put back: +0x10 .. +0x60.
const CAM_SAVED_FLOATS: usize = 20;

/// Look point above the friend's feet (chest).
const LOOK_HEIGHT: f32 = 1.4;
/// Camera distance behind the look point and height above it.
const BACK_M: f32 = 4.5;
const UP_M: f32 = 1.6;
/// Smoothing rates (1/s): about 0.15 per frame for the position and 0.08 per
/// frame for the direction at 60 fps (spec values), frame-rate independent.
const POS_RATE: f32 = 9.75;
const DIR_RATE: f32 = 5.0;
/// The friend moved this far in one frame: a teleport / warp, snap the camera.
const SNAP_M: f32 = 15.0;
/// No usable friend position for this long: give the camera back (the wait goes on).
const FRIEND_LOST: Duration = Duration::from_secs(2);
const DEFAULT_FOV: f32 = 0.75;

/// ChrCam = `*(*(base+0x3D5C0A0) + 0x30)` (a SprjPersCam; forward row at +0x30). The game's
/// camera input keeps driving it while we override the render camera [ARV/VAR].
const FIELD_CHR_CAM: usize = 0x30;
const CHRCAM_FWD: usize = 0x30;

/// Orbit pitch limits (camera elevation above the look point).
const PITCH_MIN: f32 = -35.0 * std::f32::consts::PI / 180.0;
const PITCH_MAX: f32 = 60.0 * std::f32::consts::PI / 180.0;
/// Lock-on: look point blend towards the target, and the extra camera elevation.
const LOCK_BLEND: f32 = 0.35;
const LOCK_EXTRA_PITCH: f32 = 10.0 * std::f32::consts::PI / 180.0;
/// Lock-on candidates: enemies within this of the friend; a lock is dropped past `LOCK_DROP_M`.
const LOCK_ENEMY_M: f32 = 40.0;
const LOCK_DROP_M: f32 = 60.0;
const TARGETS_EVERY: Duration = Duration::from_millis(500);

static TRAMP: AtomicUsize = AtomicUsize::new(0);
/// Fast path for the hook: the camera is wanted or still switched on.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static SHARED: Mutex<Shared> = Mutex::new(Shared::new());
/// The locked target's lock point (world), for the overlay's lock-on dot (v8.10: players
/// could not tell whether the lock was on and toggled it twice, live 2026-10-25).
static LOCK_MARK: Mutex<Option<[f32; 3]>> = Mutex::new(None);

fn set_lock_mark(p: Option<[f32; 3]>) {
    if let Ok(mut m) = LOCK_MARK.lock() {
        *m = p;
    }
}

/// ui.rs: where to draw the lock-on dot (None = no lock).
pub fn lock_mark() -> Option<[f32; 3]> {
    LOCK_MARK.lock().ok().and_then(|m| *m)
}

#[derive(Clone, Copy)]
struct Pose {
    rows: [[f32; 4]; 4],
    fov: f32,
}

/// State between the frame task and the GameRend hook.
struct Shared {
    /// Frame task: the spectator camera should be on.
    want: bool,
    /// GameRend of the pointer chain as the frame task last read it.
    target: usize,
    pose: Option<Pose>,
    /// GameRend whose `+0xE0` we switched on (0 = none) and its DebugCam.
    on: usize,
    debug_cam: usize,
    old_e0: i32,
    saved: [f32; CAM_SAVED_FLOATS],
    // Counters / last values for the frame task's log lines.
    turned_on: u32,
    restored: u32,
    no_debug_cam: bool,
    /// A GameRend the hook saw while we wanted the camera, other than `target`.
    other_rend: usize,
}

impl Shared {
    const fn new() -> Self {
        Shared {
            want: false,
            target: 0,
            pose: None,
            on: 0,
            debug_cam: 0,
            old_e0: 0,
            saved: [0.0; CAM_SAVED_FLOATS],
            turned_on: 0,
            restored: 0,
            no_debug_cam: false,
            other_rend: 0,
        }
    }

    fn publish_active(&self) {
        ACTIVE.store(self.want || self.on != 0, Ordering::Release);
    }

    /// In the hook, before the original GameRend::Update(`rend`).
    fn apply(&mut self, rend: usize) {
        let ours = self.target != 0 && rend == self.target;
        if self.on != 0 && self.on != rend {
            if !ours {
                // Another GameRend: ours is put back when it runs (or by restore_now).
                return;
            }
            // The chain now leads here, so the one we switched was replaced (a new
            // GameRend starts with E0 = 0, ctor 0x839CC0): nothing to put back.
            self.on = 0;
            self.publish_active();
        }
        if self.want && ours && let Some(pose) = self.pose {
            if self.on == 0 {
                let Some(dbg) = read_ptr(rend + REND_DEBUG_CAM) else {
                    self.no_debug_cam = true;
                    return;
                };
                let (Some(old), Some(saved)) =
                    (read::<i32>(rend + REND_FREE_CAM), read::<[f32; CAM_SAVED_FLOATS]>(dbg + CAM_ROWS))
                else {
                    return;
                };
                self.old_e0 = old;
                self.saved = saved;
                self.debug_cam = dbg;
                self.on = rend;
                self.turned_on = self.turned_on.wrapping_add(1);
            }
            // The DebugCam pointer is fixed for a GameRend's life (ctor only); a change
            // means the object is not what we think: put E0 back and stop.
            if read_ptr(rend + REND_DEBUG_CAM) != Some(self.debug_cam) {
                self.want = false;
                self.give_back(rend);
                return;
            }
            write(self.debug_cam + CAM_ROWS, pose.rows);
            write(self.debug_cam + CAM_FOV, pose.fov);
            write(rend + REND_FREE_CAM, 1i32);
        } else if self.on == rend {
            self.give_back(rend);
        } else if self.want && !ours {
            self.other_rend = rend;
        }
    }

    /// `+0xE0` and the DebugCam as they were before we switched it on.
    fn give_back(&mut self, rend: usize) {
        if read_ptr(rend + REND_DEBUG_CAM) == Some(self.debug_cam) {
            write(self.debug_cam + CAM_ROWS, self.saved);
        }
        write(rend + REND_FREE_CAM, self.old_e0);
        self.on = 0;
        self.restored = self.restored.wrapping_add(1);
        self.publish_active();
    }
}

type RendUpdate = extern "C" fn(usize, f32) -> usize;

/// `0x83B150(GameRend*, float dt)`: dt arrives in xmm1 (moved to xmm6 at entry).
extern "C" fn rend_update_detour(rend: usize, dt: f32) -> usize {
    if ACTIVE.load(Ordering::Acquire) {
        // A frame-task panic while it held the lock poisons it; the camera must
        // still be given back then, so a poisoned lock is used as is.
        let guard = match SHARED.try_lock() {
            Ok(g) => Some(g),
            Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        if let Some(mut s) = guard {
            if !crate::mod_alive() {
                // The mod gave up: only ever put the camera back.
                s.want = false;
            }
            s.apply(rend);
        }
    }
    let t = TRAMP.load(Ordering::Acquire);
    if t == 0 {
        // Hooked but the trampoline is not stored yet (install window): skip one update.
        return 0;
    }
    let f: RendUpdate = unsafe { std::mem::transmute(t) };
    f(rend, dt)
}

pub fn install() -> bool {
    let detour: RendUpdate = rend_update_detour;
    match hook::install_detour(REND_UPDATE_RVA, &REND_UPDATE_PROLOGUE, detour as usize, "GameRend::Update (0x83B150)") {
        Some(t) => {
            TRAMP.store(t, Ordering::Release);
            true
        }
        None => false,
    }
}

pub fn installed() -> bool {
    TRAMP.load(Ordering::Acquire) != 0
}

fn chain_game_rend() -> Option<usize> {
    read_ptr(read_ptr(module_base() + FIELD_AREA_RVA)? + FIELD_GAME_REND)
}

/// Gives the camera back now, from the frame task (before a reload, a load, a warp).
/// Writes only when the pointer chain still leads to the GameRend we switched.
/// Returns true when it put a value back. Also called by `bossrules::emergency_reset`
/// after a frame-task panic, when no `SpectateCam` is engaged any more.
pub(crate) fn restore_now() -> bool {
    let mut s = SHARED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // Everything in Shared is plain data that stays consistent field by field;
    // clear the flag so `update` / `drain_log` work again after a panic.
    SHARED.clear_poison();
    s.want = false;
    s.pose = None;
    set_lock_mark(None);
    let chain = chain_game_rend();
    s.target = chain.unwrap_or(0);
    let mut wrote = false;
    if s.on != 0 && chain == Some(s.on) {
        let rend = s.on;
        s.give_back(rend);
        wrote = true;
    }
    // Otherwise the chain does not lead to it (loading): `on` stays, and the hook
    // puts it back if that GameRend is updated again, or forgets it once the
    // chain's new GameRend is updated.
    s.publish_active();
    wrote
}

/// Which way an `angle` (physics yaw, radians) points in the xz plane, learned
/// like enemies.rs `Facing`: while a character runs, its movement direction is
/// scored against the 8 candidate mappings.
#[derive(Default)]
struct Facing {
    /// Previous position per character: 0 = our player, `1 + PeerId::index` for each peer.
    prev: [Option<[f32; 3]>; 1 + MAX_PLAYERS],
    score: [f32; 8],
    samples: u32,
}

impl Facing {
    fn candidates(a: f32) -> [[f32; 2]; 8] {
        let (s, c) = a.sin_cos();
        [[s, c], [-s, c], [s, -c], [-s, -c], [c, s], [-c, s], [c, -s], [-c, -s]]
    }

    /// `who`: an index into `prev` (separate previous positions per character).
    fn learn(&mut self, who: usize, pos: [f32; 3], angle: f32) {
        if who >= self.prev.len() || !game::sane_pos(pos) || !game::sane_angle(angle) {
            return;
        }
        if let Some(p) = self.prev[who] {
            let (dx, dz) = (pos[0] - p[0], pos[2] - p[2]);
            let len = (dx * dx + dz * dz).sqrt();
            // 0.02..1.0 m per frame: walking or running, not standing or a teleport.
            if (0.02..1.0).contains(&len) && self.samples < 100_000 {
                for (score, f) in self.score.iter_mut().zip(Self::candidates(angle)) {
                    *score += (f[0] * dx + f[1] * dz) / len;
                }
                self.samples += 1;
            }
        }
        self.prev[who] = Some(pos);
    }

    fn best(&self) -> Option<usize> {
        (self.samples >= 30).then(|| (0..8).max_by(|&a, &b| self.score[a].total_cmp(&self.score[b])).unwrap_or(0))
    }

    fn forward(&self, angle: f32) -> Option<[f32; 2]> {
        Some(Self::candidates(angle)[self.best()?])
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn norm(a: [f32; 3]) -> Option<[f32; 3]> {
    let l = dot(a, a).sqrt();
    (l > 1e-4 && l.is_finite()).then(|| [a[0] / l, a[1] / l, a[2] / l])
}

fn clamp_pitch(p: f32) -> f32 {
    p.clamp(PITCH_MIN, PITCH_MAX)
}

/// Camera position on the orbit sphere around `look`.
fn orbit_cam(look: [f32; 3], dist: f32, yaw: f32, pitch: f32) -> [f32; 3] {
    let (sp, cp) = pitch.sin_cos();
    [look[0] + dist * cp * yaw.cos(), look[1] + dist * sp, look[2] + dist * cp * yaw.sin()]
}

/// (yaw, pitch) of the camera as seen from the look point, from the offset `cam - look`.
fn orbit_angles(d: [f32; 3]) -> Option<(f32, f32)> {
    let h = (d[0] * d[0] + d[2] * d[2]).sqrt();
    (d.iter().all(|v| v.is_finite()) && (h > 1e-3 || d[1].abs() > 1e-3)).then(|| (d[2].atan2(d[0]), clamp_pitch(d[1].atan2(h))))
}

/// Input turn applied to the orbit. `d_pitch` is the change of the orbit pitch.
fn apply_orbit(yaw: f32, pitch: f32, d_yaw: f32, d_pitch: f32) -> (f32, f32) {
    (wrap_pi(yaw + d_yaw), clamp_pitch(pitch + d_pitch))
}

/// No steering for `secs` seconds (0 = the feature is off): swing behind the friend.
fn recenter_due(since_input: Option<Duration>, secs: u32) -> bool {
    secs > 0 && since_input.is_some_and(|d| d >= Duration::from_secs(secs as u64))
}

/// Lock-on framing for the friend's look point `fl` and the aim point `t`: (look point,
/// orbit yaw, orbit pitch). The camera sits behind the friend, opposite the target, a little
/// above the line target -> friend. None when the target is straight above / below.
fn lock_goal(fl: [f32; 3], t: [f32; 3]) -> Option<([f32; 3], f32, f32)> {
    let (dx, dz) = (t[0] - fl[0], t[2] - fl[2]);
    let hd = (dx * dx + dz * dz).sqrt();
    if hd < 0.3 || !t.iter().all(|v| v.is_finite()) {
        return None;
    }
    let look = [0, 1, 2].map(|i| fl[i] + (t[i] - fl[i]) * LOCK_BLEND);
    Some((look, (-dz).atan2(-dx), clamp_pitch((fl[1] - t[1]).atan2(hd) + LOCK_EXTRA_PITCH)))
}

/// A lock candidate as read from the game.
#[derive(Clone, Copy)]
struct Raw {
    id: usize,
    pos: Option<[f32; 3]>,
    hp: Option<i32>,
    boss: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Cand {
    id: usize,
    pos: [f32; 3],
    boss: bool,
}

/// The boss first (alive, within `LOCK_DROP_M` of the friend), then the living enemies within
/// `LOCK_ENEMY_M`, nearest first. A chr listed twice (the boss is also an enemy) counts once.
fn build_targets(friend: [f32; 3], raw: &[Raw]) -> Vec<Cand> {
    let mut out: Vec<(f32, Cand)> = Vec::new();
    for r in raw {
        let (Some(pos), Some(hp)) = (r.pos, r.hp) else { continue };
        if hp <= 0 || !game::sane_pos(pos) {
            continue;
        }
        let d = game::distance(pos, friend);
        if d > if r.boss { LOCK_DROP_M } else { LOCK_ENEMY_M } {
            continue;
        }
        match out.iter_mut().find(|(_, c)| c.id == r.id) {
            Some((_, c)) => c.boss |= r.boss,
            None => out.push((d, Cand { id: r.id, pos, boss: r.boss })),
        }
    }
    out.sort_by(|a, b| b.1.boss.cmp(&a.1.boss).then(a.0.total_cmp(&b.0)));
    out.into_iter().map(|(_, c)| c).collect()
}

/// Next target to the right (`dir` > 0) or left of `cur` as seen from the camera `cam`
/// (screen yaw relative to the current target; `hand` as in `look_at`). Past the last one
/// it wraps to the far end. None when there is no other target.
fn cycle_target(targets: &[Cand], cur: usize, cur_pos: [f32; 3], cam: [f32; 3], hand: f32, dir: i8) -> Option<usize> {
    let yaw_of = |p: [f32; 3]| (p[2] - cam[2]).atan2(p[0] - cam[0]);
    let base = yaw_of(cur_pos);
    // Positive = to the right of the current target on screen.
    let offs: Vec<(f32, usize)> =
        targets.iter().filter(|c| c.id != cur).map(|c| (-hand * wrap_pi(yaw_of(c.pos) - base), c.id)).collect();
    let right: Vec<_> = offs.iter().filter(|o| o.0 > 1e-4).collect();
    let left: Vec<_> = offs.iter().filter(|o| o.0 <= 1e-4).collect();
    let nearest = |v: &[&(f32, usize)]| v.iter().min_by(|a, b| a.0.abs().total_cmp(&b.0.abs())).map(|o| o.1);
    let farthest = |v: &[&(f32, usize)]| v.iter().max_by(|a, b| a.0.abs().total_cmp(&b.0.abs())).map(|o| o.1);
    if dir > 0 { nearest(&right).or_else(|| farthest(&left)) } else { nearest(&left).or_else(|| farthest(&right)) }
}

/// Why a lock must go: unreadable, dead or too far from the friend.
fn drop_reason(hp: Option<i32>, pos: Option<[f32; 3]>, friend: [f32; 3]) -> Option<&'static str> {
    let (Some(hp), Some(pos)) = (hp, pos.filter(|p| game::sane_pos(*p))) else { return Some("unreadable") };
    if hp <= 0 {
        Some("dead")
    } else if game::distance(pos, friend) > LOCK_DROP_M {
        Some("too far")
    } else {
        None
    }
}

/// Forward row of the game's camera (ChrCam).
fn chrcam_fwd() -> Option<[f32; 3]> {
    let cam = read_ptr(read_ptr(module_base() + FIELD_AREA_RVA)? + FIELD_CHR_CAM)?;
    read::<[f32; 3]>(cam + CHRCAM_FWD)
}

/// The render camera now: rows and fov (None on a failed or insane read).
fn render_camera() -> Option<([[f32; 4]; 4], f32)> {
    let cam = read_ptr(chain_game_rend()? + REND_CAM)?;
    let rows: [[f32; 4]; 4] = read(cam + CAM_ROWS)?;
    let fov: f32 = read(cam + CAM_FOV)?;
    (rows.iter().flatten().all(|v| v.is_finite())).then_some((rows, fov))
}

/// Camera rows for a camera at `c` looking at `t`, with the game's handedness `hand`.
fn look_at(c: [f32; 3], t: [f32; 3], hand: f32) -> Option<[[f32; 4]; 4]> {
    let world_up = [0.0, 1.0, 0.0];
    let fwd = norm(sub(t, c))?;
    let r = norm(cross(world_up, fwd))?;
    let right = [r[0] * hand, r[1] * hand, r[2] * hand];
    let d = dot(world_up, fwd);
    let up = norm([-fwd[0] * d, 1.0 - fwd[1] * d, -fwd[2] * d])?;
    Some([
        [right[0], right[1], right[2], 0.0],
        [up[0], up[1], up[2], 0.0],
        [fwd[0], fwd[1], fwd[2], 0.0],
        [c[0], c[1], c[2], 1.0],
    ])
}

/// Which peer the spectator camera follows (PLAN-MULTIPLAYER §2 bossrules/spectate):
/// 1. the current target while it is alive, `engaged` and drawn (has a view);
/// 2. else the nearest living, engaged, drawn peer (to `from`: our camera or our body;
///    a tie, or no `from`, goes to the lowest id);
/// 3. else the current target while it is still drawn (it died too: the wipe / release
///    rules decide, exactly as with two players);
/// 4. else None.
pub fn choose_target(
    peers: &PeerSet,
    current: Option<PeerId>,
    from: Option<[f32; 3]>,
    engaged: &dyn Fn(&PeerFrame) -> bool,
) -> Option<PeerId> {
    let drawn = |p: &PeerFrame| p.view.is_some_and(|(q, _)| q.iter().all(|v| v.is_finite()));
    let cur = current.and_then(|id| peers.by_id(id)).filter(|p| drawn(p));
    if let Some(p) = cur.filter(|p| p.alive() && engaged(p)) {
        return Some(p.id);
    }
    let mut best: Option<(f32, PeerId)> = None;
    for p in peers.iter().filter(|p| p.alive() && engaged(p) && drawn(p)) {
        let d = match (from, p.view) {
            (Some(f), Some((q, _))) => (0..3).map(|i| (q[i] - f[i]) * (q[i] - f[i])).sum::<f32>(),
            _ => 0.0,
        };
        let d = if d.is_finite() { d } else { f32::MAX };
        // Strictly closer only: the earlier (lower) id keeps a tie.
        if best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, p.id));
        }
    }
    best.map(|(_, id)| id).or(cur.map(|p| p.id))
}

/// The switch input (multiplayer): the next (`dir` > 0) or previous living, engaged, drawn
/// peer after `current` in id order, wrapping round. None when nobody else qualifies.
pub fn next_peer(peers: &PeerSet, current: Option<PeerId>, dir: i8, engaged: &dyn Fn(&PeerFrame) -> bool) -> Option<PeerId> {
    let drawn = |p: &PeerFrame| p.view.is_some_and(|(q, _)| q.iter().all(|v| v.is_finite()));
    let ids: Vec<PeerId> = peers.iter().filter(|p| p.alive() && engaged(p) && drawn(p)).map(|p| p.id).collect();
    let pick = match (current, dir > 0) {
        (Some(c), true) => ids.iter().find(|&&id| id > c).or_else(|| ids.first()),
        (Some(c), false) => ids.iter().rev().find(|&&id| id < c).or_else(|| ids.last()),
        (None, true) => ids.first(),
        (None, false) => ids.last(),
    };
    pick.copied().filter(|&id| Some(id) != current)
}

/// Frame-task side: follows the friend, logs what the hook did.
#[derive(Default)]
pub struct SpectateCam {
    /// The peer `update_peers` follows (None: none chosen, or `update` is used).
    target: Option<PeerId>,
    facing: Facing,
    /// Engaged = we asked the hook for the camera (until `release`).
    engaged: bool,
    cam: Option<[f32; 3]>,
    /// Direction of the camera from the look point in the xz plane (radians, atan2(z, x)).
    dir: Option<f32>,
    look: Option<[f32; 3]>,
    /// +1 / -1: sign of (up x forward) . right of the game's own camera.
    hand: f32,
    fov: f32,
    last_at: Option<Instant>,
    friend_seen_at: Option<Instant>,
    logged_on: u32,
    logged_restored: u32,
    logged_no_debug: bool,
    logged_other: usize,
    hook_missing_logged: bool,
    // Orbit / lock-on (spectate_orbit != 0), IMPLEMENTATION-NOTES §41.
    orbit_ready: bool,
    orbit_yaw: f32,
    orbit_pitch: f32,
    /// Displayed pitch (smoothed while locked).
    pitch_s: f32,
    friend_look: Option<[f32; 3]>,
    engaged_at: Option<Instant>,
    poller: input::Poller,
    /// This frame's input (`update_peers` samples once; `step_orbit` uses it).
    sample: Option<input::Sample>,
    /// Switch input last frame (next, previous) and the last switch.
    switch_prev: (bool, bool),
    switch_at: Option<Instant>,
    /// Game camera angles of the previous frame (source A).
    last_a: Option<(f32, f32)>,
    a_moved: bool,
    a_off_logged: bool,
    /// Source A was frozen: the stick is used (this engage).
    auto_b: bool,
    flick: input::Flick,
    stick_state: i8,
    lock_prev: bool,
    lock_at: Option<Instant>,
    /// Chr (pointer) we are locked on.
    lock: Option<usize>,
    targets: Vec<Cand>,
    targets_at: Option<Instant>,
    last_input_at: Option<Instant>,
}

impl SpectateCam {
    /// Every frame in the world. `want`: we are dead and spectate with the camera
    /// allowed. `friend`: where the friend is drawn (position, yaw). `boss_hint`: entity id
    /// of the boss of this fight (lock-on candidate number one).
    pub fn update(
        &mut self,
        want: bool,
        friend: Option<([f32; 3], f32)>,
        friend_name: &str,
        boss_hint: Option<i32>,
    ) {
        let now = Instant::now();
        self.drain_log();
        if !want {
            if self.engaged {
                self.release("spectating ended");
            }
            return;
        }
        if !installed() {
            if !self.hook_missing_logged {
                self.hook_missing_logged = true;
                log::line("spectate: camera hook not installed, death screen only");
            }
            return;
        }
        let friend = friend.filter(|(p, a)| game::sane_pos(*p) && game::sane_angle(*a));
        let Some((pos, angle)) = friend else {
            if self.engaged && self.friend_seen_at.is_none_or(|t| now.duration_since(t) >= FRIEND_LOST) {
                self.release("friend position lost");
            }
            return;
        };
        self.friend_seen_at = Some(now);
        let dt = self.last_at.map_or(1.0 / 60.0, |t| now.duration_since(t).as_secs_f32().clamp(0.0, 0.25));
        self.last_at = Some(now);

        if !self.engaged {
            self.engaged = true;
            self.cam = None;
            self.dir = None;
            self.look = None;
            self.hand = 1.0;
            self.fov = DEFAULT_FOV;
            self.reset_orbit(now);
            // Start where the game's camera is, with its handedness and fov.
            if let Some((rows, fov)) = render_camera() {
                let r = [rows[0][0], rows[0][1], rows[0][2]];
                let u = [rows[1][0], rows[1][1], rows[1][2]];
                let f = [rows[2][0], rows[2][1], rows[2][2]];
                if dot(cross(u, f), r) < 0.0 {
                    self.hand = -1.0;
                }
                if (0.3..=1.6).contains(&fov) {
                    self.fov = fov;
                }
                self.cam = Some([rows[3][0], rows[3][1], rows[3][2]]).filter(|&c| game::sane_pos(c));
            }
            log::line(&format!(
                "spectate: camera requested, following {friend_name} (hand {}, fov {:.2}, facing mapping {:?})",
                self.hand,
                self.fov,
                self.facing.best()
            ));
        }

        let mode = tuning::spectate_orbit();
        let (cam, look) = if mode == 0 {
            self.orbit_ready = false;
            self.step_fixed(dt, pos, angle)
        } else {
            self.step_orbit(mode, now, dt, pos, angle, boss_hint)
        };
        if cam.iter().all(|v| v.is_finite()) {
            self.cam = Some(cam);
        }
        let Some(rows) = look_at(cam, look, self.hand) else { return };
        let target = chain_game_rend().unwrap_or(0);
        if let Ok(mut s) = SHARED.lock() {
            s.want = target != 0;
            s.target = target;
            s.pose = Some(Pose { rows, fov: self.fov });
            s.publish_active();
        }
    }

    /// Fresh orbit / lock state for a new engage (and when it ends).
    fn reset_orbit(&mut self, now: Instant) {
        self.orbit_ready = false;
        self.friend_look = None;
        self.engaged_at = Some(now);
        self.last_a = None;
        self.a_moved = false;
        self.a_off_logged = false;
        self.auto_b = false;
        self.flick.reset();
        self.stick_state = 0;
        self.lock_prev = false;
        self.lock = None;
        set_lock_mark(None);
        self.targets.clear();
        self.targets_at = None;
        self.last_input_at = Some(now);
    }

    /// The old camera (`spectate_orbit = 0`): fixed behind the friend. Returns (cam, look).
    fn step_fixed(&mut self, dt: f32, pos: [f32; 3], angle: f32) -> ([f32; 3], [f32; 3]) {
        let look = [pos[0], pos[1] + LOOK_HEIGHT, pos[2]];
        let snap = self.look.is_none_or(|l| game::distance(l, look) > SNAP_M)
            || self.cam.is_none_or(|c| game::distance(c, look) > 3.0 * SNAP_M);
        self.look = Some(look);
        // Behind the friend once the yaw convention is known; until then (or if it
        // is never learned) the camera trails: it keeps its side and distance.
        let behind = self.facing.forward(angle).map(|f| (-f[1]).atan2(-f[0]));
        let trailing = self.cam.and_then(|c| {
            let (dx, dz) = (c[0] - look[0], c[2] - look[2]);
            (dx * dx + dz * dz > 1e-4).then(|| dz.atan2(dx))
        });
        let goal_dir = behind.or(trailing).or(self.dir).unwrap_or(0.0);
        let dir = match self.dir {
            Some(d) if !snap => {
                let k = 1.0 - (-DIR_RATE * dt).exp();
                wrap_pi(d + wrap_pi(goal_dir - d) * k)
            }
            _ => goal_dir,
        };
        self.dir = Some(dir);
        let goal = [look[0] + BACK_M * dir.cos(), look[1] + UP_M, look[2] + BACK_M * dir.sin()];
        let cam = match self.cam {
            Some(c) if !snap => {
                let k = 1.0 - (-POS_RATE * dt).exp();
                [0, 1, 2].map(|i| c[i] + (goal[i] - c[i]) * k)
            }
            _ => goal,
        };
        (cam, look)
    }

    /// Rebuilds the lock candidates (boss first, then enemies near the friend).
    fn refresh_targets(&mut self, friend: [f32; 3], hint: Option<i32>, now: Instant) {
        self.targets_at = Some(now);
        let mut raw = Vec::new();
        if let Some(c) = hint.filter(|&e| e > 0).and_then(game::chr_by_entity_id) {
            raw.push(Raw { id: c.0, pos: c.position(), hp: c.hp(), boss: true });
        }
        for c in crate::nsession::enemy_chrs() {
            raw.push(Raw { id: c.0, pos: c.position(), hp: c.hp(), boss: false });
        }
        self.targets = build_targets(friend, &raw);
    }

    fn describe(&self, id: usize) -> String {
        let boss = self.targets.iter().any(|c| c.id == id && c.boss);
        format!("{} ({})", Chr(id).entity_id().unwrap_or(0), if boss { "boss" } else { "enemy" })
    }

    /// Drops the lock; the free orbit continues from the camera as it is shown.
    fn unlock(&mut self, why: &str) {
        set_lock_mark(None);
        if self.lock.take().is_some() {
            log::line(&format!("spectate: lock lost ({why})"));
        }
        self.orbit_yaw = self.dir.unwrap_or(self.orbit_yaw);
        self.orbit_pitch = clamp_pitch(self.pitch_s);
        self.flick.reset();
    }

    /// The orbit camera (`spectate_orbit` 1 or 2) with lock-on. Returns (cam, look).
    fn step_orbit(
        &mut self,
        mode: u8,
        now: Instant,
        dt: f32,
        pos: [f32; 3],
        angle: f32,
        hint: Option<i32>,
    ) -> ([f32; 3], [f32; 3]) {
        let look_f = [pos[0], pos[1] + LOOK_HEIGHT, pos[2]];
        let snap = self.friend_look.is_none_or(|l| game::distance(l, look_f) > SNAP_M)
            || self.cam.is_none_or(|c| game::distance(c, self.look.unwrap_or(look_f)) > 3.0 * SNAP_M);
        self.friend_look = Some(look_f);
        let behind = self.facing.forward(angle).map(|f| (-f[1]).atan2(-f[0]));
        if !self.orbit_ready {
            // Start where the game's camera is (direction only, the distance is ours).
            let start = self.cam.and_then(|c| orbit_angles(sub(c, look_f)));
            let (y, p) = start.unwrap_or_else(|| (behind.unwrap_or(0.0), UP_M.atan2(BACK_M)));
            self.orbit_yaw = y;
            self.orbit_pitch = p;
            self.pitch_s = p;
            self.dir = Some(y);
            self.look = Some(look_f);
            self.orbit_ready = true;
        }
        let hand = self.hand;
        let sample = match self.sample.take() {
            Some(s) => s,
            None => self.poller.sample(tuning::spectate_lock_key(), tuning::spectate_switch_key(), now),
        };
        let engaged_for = now.duration_since(self.engaged_at.unwrap_or(now));
        let t = engaged_for.as_secs_f32();

        // --- Steering: source A (the game's camera) and/or B (the stick). ---
        let mut turn = (0.0f32, 0.0f32); // (yaw of the view, orbit pitch)
        let mut cycle: Option<i8> = None;
        let mut a_turn = 0.0f32;
        if mode == 1 {
            match chrcam_fwd().and_then(input::fwd_angles) {
                Some(cur) => {
                    if let Some(last) = self.last_a
                        && let Some((dy, dp)) = input::angle_delta(last, cur)
                        && input::is_motion(dy, dp)
                    {
                        if !self.a_moved {
                            self.a_moved = true;
                            log::line("spectate: orbit input from the game camera");
                        }
                        // Looking up = the camera swings down: the orbit pitch falls.
                        turn = (dy, -dp);
                        a_turn = dy;
                    }
                    self.last_a = Some(cur);
                }
                None => {
                    if !self.a_off_logged {
                        self.a_off_logged = true;
                        log::line("spectate: ChrCam not readable, game camera input off");
                    }
                }
            }
            if !self.auto_b && input::frozen_after(engaged_for, self.a_moved) {
                self.auto_b = true;
                log::line("spectate: game camera frozen while dead, using XInput / mouse / arrows");
            }
        }
        if mode == 2 || self.auto_b {
            // Orbit axes: the pad stick, else mouse look / arrow keys (sample.look).
            let (sx, sy) = sample.look;
            let (dy, dp) = input::stick_orbit(sx, sy, dt, tuning::spectate_orbit_speed(), hand);
            turn = (turn.0 + dy, turn.1 + dp);
            // Target cycling stays on the pad stick edge only.
            if let Some((px, _)) = sample.stick {
                let (s, c) = input::stick_edge(self.stick_state, px);
                self.stick_state = s;
                cycle = c;
            }
        }

        // --- Lock toggle. ---
        let pressed = input::edge(self.lock_prev, sample.lock_down);
        self.lock_prev = sample.lock_down;
        if !tuning::spectate_lock() {
            if self.lock.is_some() {
                self.unlock("disabled");
            }
        } else {
            if pressed || self.targets_at.is_none_or(|a| now.duration_since(a) >= TARGETS_EVERY) {
                self.refresh_targets(pos, hint, now);
            }
            if pressed && input::debounce_ok(self.lock_at.map(|a| now.duration_since(a))) {
                self.lock_at = Some(now);
                if self.lock.is_some() {
                    self.unlock("toggled off");
                } else if let Some(c) = self.targets.first().copied() {
                    self.lock = Some(c.id);
                    self.flick.reset();
                    log::line(&format!("spectate: lock on {}", self.describe(c.id)));
                }
            }
        }

        // --- Keep the lock valid. ---
        let mut aim: Option<[f32; 3]> = None;
        if let Some(id) = self.lock {
            let c = Chr(id);
            let p = c.position();
            match drop_reason(c.hp(), p, pos) {
                None => aim = p,
                Some(why) => {
                    self.refresh_targets(pos, hint, now);
                    if let Some(n) = self.targets.iter().find(|c| c.id != id).map(|c| c.id) {
                        log::line(&format!("spectate: lock lost ({why})"));
                        self.lock = Some(n);
                        self.flick.reset();
                        aim = Chr(n).position();
                        log::line(&format!("spectate: lock switch to {}", self.describe(n)));
                    } else {
                        self.unlock(why);
                    }
                }
            }
        }

        // --- Cycling while locked: a camera flick (source A) or the stick edge (B). ---
        if self.lock.is_some() && mode == 1 {
            cycle = cycle.or(self.flick.push(t, -hand * a_turn));
        }
        if let (Some(dir), Some(cur), Some(cur_pos)) = (cycle, self.lock, aim)
            && let Some(n) = cycle_target(&self.targets, cur, cur_pos, self.cam.unwrap_or(look_f), hand, dir)
        {
            self.lock = Some(n);
            aim = Chr(n).position();
            log::line(&format!("spectate: lock switch to {}", self.describe(n)));
        }
        if input::is_motion(turn.0, turn.1) || cycle.is_some() {
            self.last_input_at = Some(now);
        }
        set_lock_mark(self.lock.and(aim).map(|a| [a[0], a[1] + tuning::spectate_lock_height(), a[2]]));

        // --- Where the camera wants to be. ---
        let k_dir = 1.0 - (-DIR_RATE * dt).exp();
        let k_pos = 1.0 - (-POS_RATE * dt).exp();
        let (cur_dir, cur_pitch) = (self.dir.unwrap_or(self.orbit_yaw), self.pitch_s);
        let framing = self
            .lock
            .and(aim)
            .and_then(|a| lock_goal(look_f, [a[0], a[1] + tuning::spectate_lock_height(), a[2]]));
        let (goal_look, dir, pitch) = if let Some((gl, gy, gp)) = framing {
            if snap {
                (gl, gy, gp)
            } else {
                (gl, wrap_pi(cur_dir + wrap_pi(gy - cur_dir) * k_dir), clamp_pitch(cur_pitch + (gp - cur_pitch) * k_dir))
            }
        } else if self.lock.is_some() {
            (look_f, cur_dir, cur_pitch) // locked but not frameable this frame: hold
        } else {
            let (mut y, p) = apply_orbit(self.orbit_yaw, self.orbit_pitch, turn.0, turn.1);
            if recenter_due(self.last_input_at.map(|a| now.duration_since(a)), tuning::spectate_recenter_s())
                && let Some(b) = behind
            {
                y = wrap_pi(y + wrap_pi(b - y) * k_dir);
            }
            self.orbit_yaw = y;
            self.orbit_pitch = p;
            (look_f, y, p)
        };
        if !(dir.is_finite() && pitch.is_finite() && goal_look.iter().all(|v| v.is_finite())) {
            return (self.cam.unwrap_or(look_f), self.look.unwrap_or(look_f));
        }
        self.dir = Some(dir);
        self.pitch_s = pitch;
        let look = match self.look {
            Some(l) if !snap => [0, 1, 2].map(|i| l[i] + (goal_look[i] - l[i]) * k_pos),
            _ => goal_look,
        };
        self.look = Some(look);
        let goal = orbit_cam(look, BACK_M, dir, pitch);
        let cam = match self.cam {
            Some(c) if !snap => [0, 1, 2].map(|i| c[i] + (goal[i] - c[i]) * k_pos),
            _ => goal,
        };
        (cam, look)
    }

    /// Every frame: the convention of `angle` is learned from our own player (alive) and
    /// from every peer drawn this frame, each with its own previous position.
    pub fn learn_peers(&mut self, player: Chr, alive: bool, peers: &PeerSet) {
        if alive && let (Some(p), Some(a)) = (player.position(), player.angle()) {
            self.facing.learn(0, p, a);
        }
        for p in peers.iter() {
            if let Some((pos, a)) = p.view {
                self.facing.learn(1 + p.id.index(), pos, a);
            }
        }
    }

    /// Multi-player `update`: follows the peer `choose_target` picks among the ones
    /// `engaged` accepts (P4: living peers in the same fight), and switches to the nearest
    /// other one when it dies or leaves the fight (logged). The switch input
    /// (`spectate_switch_key`, D-pad right / left) moves it to the next / previous living one
    /// by id (§5.20). With one peer this is `update` on that peer's view. `from`: where
    /// "nearest" is measured from (our body). `boss_hint`: lock-on candidate number one.
    pub fn update_peers(
        &mut self,
        want: bool,
        peers: &PeerSet,
        from: Option<[f32; 3]>,
        engaged: &dyn Fn(&PeerFrame) -> bool,
        boss_hint: Option<i32>,
    ) {
        let now = Instant::now();
        let name = |id: Option<PeerId>| {
            id.map_or_else(|| "nobody".to_string(), |id| peers.by_id(id).map_or_else(|| format!("slot {}", id.slot()), |p| p.name.clone()))
        };
        if want {
            let s = self.poller.sample(tuning::spectate_lock_key(), tuning::spectate_switch_key(), now);
            let dir = if input::edge(self.switch_prev.0, s.next_down) {
                1
            } else if input::edge(self.switch_prev.1, s.prev_down) {
                -1
            } else {
                0
            };
            self.switch_prev = (s.next_down, s.prev_down);
            self.sample = Some(s);
            if dir != 0
                && input::debounce_ok(self.switch_at.map(|a| now.duration_since(a)))
                && let Some(n) = next_peer(peers, self.target, dir, engaged)
            {
                self.switch_at = Some(now);
                if Some(n) != self.target {
                    log::line(&format!("spectate: watching {} (switch input; was {})", name(Some(n)), name(self.target)));
                    self.target = Some(n);
                }
            }
        } else {
            self.switch_prev = (false, false);
            self.sample = None;
        }
        let next = if want { choose_target(peers, self.target, self.cam.or(from), engaged) } else { None };
        if want && next != self.target && self.target.is_some() {
            log::line(&format!("spectate: camera switches from {} to {}", name(self.target), name(next)));
        }
        self.target = next;
        let p = next.and_then(|id| peers.by_id(id));
        let view = p.and_then(|p| p.view);
        let name = p.map_or("Friend", |p| p.name.as_str()).to_string();
        self.update(want, view, &name, boss_hint);
    }

    /// The peer `update_peers` follows now.
    pub fn target(&self) -> Option<PeerId> {
        self.target
    }

    /// Gives the camera back now (any end of spectating: release, load, warp).
    pub fn release(&mut self, why: &str) {
        let was = self.engaged;
        self.target = None;
        if self.lock.is_some() {
            log::line("spectate: lock lost (released)");
        }
        self.reset_orbit(Instant::now());
        self.engaged = false;
        self.cam = None;
        self.dir = None;
        self.look = None;
        self.last_at = None;
        self.friend_seen_at = None;
        let wrote = restore_now();
        self.drain_log();
        if was || wrote {
            log::line(&format!("spectate: camera released ({why}){}", if wrote { ", restored now" } else { "" }));
        }
    }

    /// Log lines for what the hook did since the last frame.
    fn drain_log(&mut self) {
        let Ok(s) = SHARED.lock() else { return };
        let (on, restored, no_debug, other, old, rend) =
            (s.turned_on, s.restored, s.no_debug_cam, s.other_rend, s.old_e0, s.on);
        drop(s);
        if on != self.logged_on {
            self.logged_on = on;
            log::line(&format!("spectate: camera on (GameRend {rend:#x}, old E0 = {old})"));
        }
        if restored != self.logged_restored {
            self.logged_restored = restored;
            log::line(&format!("spectate: camera restored (E0 = {old})"));
        }
        if no_debug && !self.logged_no_debug {
            self.logged_no_debug = true;
            log::line("spectate: no debug cam (GameRend+0xE8 is null), camera stays");
        }
        if other != 0 && other != self.logged_other {
            self.logged_other = other;
            log::line(&format!(
                "spectate: GameRend::Update ran on {other:#x}, the pointer chain has {:#x}",
                chain_game_rend().unwrap_or(0)
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peers::PeerFrame;

    fn peer(slot: u8, pos: [f32; 3], hp: i32) -> PeerFrame {
        PeerFrame {
            snapshot: Some(coop_net::Snapshot { pos, hp, block: 1, ..Default::default() }),
            view: Some((pos, 0.0)),
            ..PeerFrame::bare(PeerId(slot))
        }
    }

    fn set(peers: Vec<PeerFrame>) -> PeerSet {
        let mut s = PeerSet::new(PeerId(0), true);
        for p in peers {
            s.push(p);
        }
        s
    }

    #[test]
    fn camera_keeps_its_target_and_switches_to_the_nearest_on_death() {
        let all = |_: &PeerFrame| true;
        let near = [1.0, 0.0, 0.0];
        let peers = set(vec![peer(1, [10.0, 0.0, 0.0], 100), peer(2, [2.0, 0.0, 0.0], 100), peer(3, [5.0, 0.0, 0.0], 100)]);
        // Nothing followed yet: the nearest living engaged peer.
        assert_eq!(choose_target(&peers, None, Some(near), &all), Some(PeerId(2)));
        // A living engaged target is kept even when another is nearer.
        assert_eq!(choose_target(&peers, Some(PeerId(1)), Some(near), &all), Some(PeerId(1)));
        // It dies: the nearest other one.
        let peers = set(vec![peer(1, [10.0, 0.0, 0.0], 0), peer(2, [2.0, 0.0, 0.0], 100), peer(3, [5.0, 0.0, 0.0], 100)]);
        assert_eq!(choose_target(&peers, Some(PeerId(1)), Some(near), &all), Some(PeerId(2)));
        // Only engaged peers count (peer 2 is not in the fight).
        let fight = |p: &PeerFrame| p.id != PeerId(2);
        assert_eq!(choose_target(&peers, Some(PeerId(1)), Some(near), &fight), Some(PeerId(3)));
    }

    #[test]
    fn with_nobody_left_the_dead_target_stays_and_ties_go_to_the_lowest_id() {
        let all = |_: &PeerFrame| true;
        // Everyone dead: keep following the current target (the wipe rules decide).
        let peers = set(vec![peer(1, [1.0, 0.0, 0.0], 0), peer(2, [2.0, 0.0, 0.0], 0)]);
        assert_eq!(choose_target(&peers, Some(PeerId(2)), None, &all), Some(PeerId(2)));
        assert_eq!(choose_target(&peers, None, None, &all), None);
        // Equal distance (or no reference point): the lowest id.
        let peers = set(vec![peer(4, [1.0, 0.0, 0.0], 100), peer(3, [-1.0, 0.0, 0.0], 100)]);
        assert_eq!(choose_target(&peers, None, Some([0.0; 3]), &all), Some(PeerId(3)));
        assert_eq!(choose_target(&peers, None, None, &all), Some(PeerId(3)));
        // A target that is no longer drawn (left, no view) is replaced.
        let mut gone = peer(3, [0.0; 3], 100);
        gone.view = None;
        let peers = set(vec![gone, peer(4, [1.0, 0.0, 0.0], 100)]);
        assert_eq!(choose_target(&peers, Some(PeerId(3)), None, &all), Some(PeerId(4)));
    }

    #[test]
    fn the_switch_input_walks_the_living_peers_by_id() {
        let all = |_: &PeerFrame| true;
        let peers = set(vec![peer(1, [1.0, 0.0, 0.0], 100), peer(2, [2.0, 0.0, 0.0], 0), peer(3, [3.0, 0.0, 0.0], 100), peer(5, [5.0, 0.0, 0.0], 100)]);
        // Next skips the dead peer 2 and wraps; previous goes the other way.
        assert_eq!(next_peer(&peers, Some(PeerId(1)), 1, &all), Some(PeerId(3)));
        assert_eq!(next_peer(&peers, Some(PeerId(5)), 1, &all), Some(PeerId(1)));
        assert_eq!(next_peer(&peers, Some(PeerId(1)), -1, &all), Some(PeerId(5)));
        assert_eq!(next_peer(&peers, None, 1, &all), Some(PeerId(1)));
        // Only peers in the fight count; nobody else left: no switch.
        let only3 = |p: &PeerFrame| p.id == PeerId(3);
        assert_eq!(next_peer(&peers, Some(PeerId(3)), 1, &only3), None);
        // The host (slot 0) is a peer like any other for a client.
        let mut s = PeerSet::new(PeerId(2), false);
        s.push(peer(0, [0.0; 3], 100));
        s.push(peer(1, [1.0, 0.0, 0.0], 100));
        assert_eq!(next_peer(&s, Some(PeerId(1)), 1, &all), Some(PeerId(0)));
        assert_eq!(next_peer(&s, None, 1, &all), Some(PeerId(0)));
    }

    use std::f32::consts::PI;

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn orbit_position_and_angles_round_trip() {
        let look = [10.0, 2.0, -5.0];
        for &(y, p) in &[(0.0f32, 0.3f32), (2.0, -0.4), (-3.0, 0.9)] {
            let c = orbit_cam(look, 4.5, y, p);
            assert!(near(game::distance(c, look), 4.5));
            let (y2, p2) = orbit_angles(sub(c, look)).unwrap();
            assert!(near(wrap_pi(y2 - y), 0.0) && near(p2, p), "{y} {p} -> {y2} {p2}");
        }
        assert!(near(orbit_cam(look, 4.5, 0.0, 0.0)[0], 14.5));
        assert!(orbit_angles([0.0; 3]).is_none() && orbit_angles([f32::NAN, 0.0, 1.0]).is_none());
    }

    #[test]
    fn pitch_is_clamped_and_yaw_wraps() {
        let (y, p) = apply_orbit(3.0, 0.0, 0.5, 5.0);
        assert!(y < 0.0 && near(y, 3.5 - 2.0 * PI));
        assert!(near(p, PITCH_MAX));
        assert!(near(apply_orbit(0.0, 0.0, 0.0, -5.0).1, PITCH_MIN));
        assert!(near(PITCH_MIN, -35f32.to_radians()) && near(PITCH_MAX, 60f32.to_radians()));
    }

    #[test]
    fn lock_framing_puts_the_camera_behind_the_friend() {
        let fl = [0.0, 1.4, 0.0];
        // Target 10 m along +x: camera on the -x side, looking along +x.
        let (look, yaw, pitch) = lock_goal(fl, [10.0, 1.4, 0.0]).unwrap();
        assert!(near(yaw.cos(), -1.0) && near(yaw.sin(), 0.0));
        assert!(near(look[0], 3.5) && near(look[1], 1.4));
        assert!(near(pitch, LOCK_EXTRA_PITCH));
        let cam = orbit_cam(look, BACK_M, yaw, pitch);
        assert!(cam[0] < 0.0, "camera is on the far side of the friend: {cam:?}");
        // Target on +z: the camera goes to -z.
        let (_, yaw, _) = lock_goal(fl, [0.0, 1.4, 8.0]).unwrap();
        assert!(near(yaw.sin(), -1.0));
        // A target high above: the camera drops below the friend, on the target -> friend line.
        let (_, _, pitch) = lock_goal(fl, [10.0, 11.4, 0.0]).unwrap();
        assert!(pitch < 0.0 && pitch > PITCH_MIN);
        // Straight above: no framing.
        assert!(lock_goal(fl, [0.0, 9.0, 0.1]).is_none());
        assert!(lock_goal(fl, [f32::NAN, 0.0, 5.0]).is_none());
    }

    fn raw(id: usize, x: f32, hp: i32, boss: bool) -> Raw {
        Raw { id, pos: Some([x, 0.0, 0.0]), hp: Some(hp), boss }
    }

    #[test]
    fn targets_boss_first_then_nearest_within_range() {
        let f = [0.0; 3];
        let t = build_targets(
            f,
            &[
                raw(1, 30.0, 10, false),
                raw(2, 5.0, 10, false),
                raw(3, 50.0, 10, false), // too far for an enemy
                raw(4, 20.0, 0, false),  // dead
                raw(5, 55.0, 100, true), // the boss may be further
                raw(2, 5.0, 10, true),   // the same chr listed as the boss too
                Raw { id: 6, pos: None, hp: Some(5), boss: false },
                Raw { id: 7, pos: Some([1.0, 0.0, 0.0]), hp: None, boss: false },
            ],
        );
        let ids: Vec<_> = t.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![2, 5, 1]);
        assert!(t[0].boss && t[1].boss && !t[2].boss);
        // A boss past the drop distance is not a candidate.
        assert!(build_targets(f, &[raw(9, 61.0, 10, true)]).is_empty());
    }

    fn cand(id: usize, x: f32, z: f32) -> Cand {
        Cand { id, pos: [x, 0.0, z], boss: false }
    }

    #[test]
    fn cycling_picks_the_neighbour_on_screen_and_wraps() {
        // Camera at the origin looking along +x: with hand 1 the view's right is -z.
        let cam = [0.0, 0.0, 0.0];
        let t = [cand(1, 10.0, 5.0), cand(2, 10.0, 0.0), cand(3, 10.0, -5.0), cand(4, 10.0, -20.0)];
        let pos = |id: usize| t.iter().find(|c| c.id == id).unwrap().pos;
        assert_eq!(cycle_target(&t, 2, pos(2), cam, 1.0, 1), Some(3)); // right of 2: z = -5
        assert_eq!(cycle_target(&t, 2, pos(2), cam, 1.0, -1), Some(1));
        assert_eq!(cycle_target(&t, 3, pos(3), cam, 1.0, 1), Some(4));
        // Past the rightmost: wraps to the leftmost.
        assert_eq!(cycle_target(&t, 4, pos(4), cam, 1.0, 1), Some(1));
        assert_eq!(cycle_target(&t, 1, pos(1), cam, 1.0, -1), Some(4));
        // The other handedness mirrors it.
        assert_eq!(cycle_target(&t, 2, pos(2), cam, -1.0, 1), Some(1));
        // Alone: nothing to switch to.
        assert_eq!(cycle_target(&t[..1], 1, pos(1), cam, 1.0, 1), None);
    }

    #[test]
    fn lock_drop_rules() {
        let f = [0.0; 3];
        let at = |x: f32| Some([x, 0.0, 0.0]);
        assert_eq!(drop_reason(Some(10), at(5.0), f), None);
        assert_eq!(drop_reason(Some(0), at(5.0), f), Some("dead"));
        assert_eq!(drop_reason(Some(-3), at(5.0), f), Some("dead"));
        assert_eq!(drop_reason(None, at(5.0), f), Some("unreadable"));
        assert_eq!(drop_reason(Some(10), None, f), Some("unreadable"));
        assert_eq!(drop_reason(Some(10), Some([f32::NAN, 0.0, 0.0]), f), Some("unreadable"));
        assert_eq!(drop_reason(Some(10), at(60.5), f), Some("too far"));
        assert_eq!(drop_reason(Some(10), at(59.0), f), None);
    }

    #[test]
    fn recenter_only_after_the_quiet_time() {
        assert!(!recenter_due(Some(Duration::from_secs(100)), 0));
        assert!(!recenter_due(Some(Duration::from_secs(2)), 3));
        assert!(recenter_due(Some(Duration::from_secs(3)), 3));
        assert!(!recenter_due(None, 3));
    }
}
