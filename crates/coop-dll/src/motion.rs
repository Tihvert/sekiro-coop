//! Ghost animation the way replays do it (RE-FINDINGS-2 § Ghost animation,
//! part 2). Two parts, both sent in every snapshot:
//!
//! - Behavior sync states: the player's active states (attack, roll, guard,
//!   jump...) read with the game's replay recorder query `0xFED3D0`, applied
//!   to the ghost with the replay apply `0xB4E9C0`, which raises `W_<state>`
//!   only for states the ghost is not in yet (or that are fresh). The
//!   player's actions come from the HKS script and never pass the C++ raise
//!   (0xFF1D10) we hooked first, which is why that saw nothing (2026-10-22).
//! - Movement vector: what the ReplayManipulator writes for a replay ghost
//!   (horizontal direction x 0.5 walk / 1.0 run); the ghost's own behavior
//!   turns it into MoveSpeedLevel / MoveAngle, i.e. walk and run animations.
//!
//! All calls run on our FrameBegin task. Nothing here is verified live yet.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use coop_net::{MAX_SYNC_STATES, Snapshot};

use crate::game::{Chr, module_base, read, read_ptr};
use crate::log;

const CHR_MODULES: usize = 0x1FF8;
const MOD_BEHAVIOR: usize = 0x28;
const MOD_BEHAVIOR_SYNC: usize = 0x30;
const MOD_ACTION_REQUEST: usize = 0x80;
const BM_SYNC_STATES: usize = 0x60;
const QUERY_STATES_RVA: usize = 0xFED3D0;
const APPLY_STATES_RVA: usize = 0xB4E9C0;
const REPLAY_HEAP_RVA: usize = 0x3D9CE50;
/// ChrIns+0x50 -> controller; +0x358 the replay manipulator, else +0x120.
const CHR_CTRL: usize = 0x50;
const CTRL_MANIP: usize = 0x358;
const CTRL_MANIP_ALT: usize = 0x120;
const MANIP_MOVE: usize = 0x10;
const MANIP_MOVE_2: usize = 0xC0;
const AR_DASH: usize = 0x184;
const BM_MOVE_SPEED_LEVEL: usize = 0xBAC;
const BM_MOVE_ANGLE: usize = 0xBB0;
const IS_SYNC_DRIVEN_RVA: usize = 0x9F36C0;
/// Singleton whose +0x10 is the sync-state name dictionary (0x10158F0).
const SYNC_DICT_RVA: usize = 0x3D92578;
const STATS_EVERY: Duration = Duration::from_secs(30);

/// The game's growable vector as the replay code uses it.
#[repr(C)]
struct DlVec {
    alloc: usize,
    begin: usize,
    end: usize,
    cap: usize,
}

fn module(chr: Chr, slot: usize) -> Option<usize> {
    read_ptr(read_ptr(chr.0 + CHR_MODULES)? + slot).filter(|&p| p >= 0x10000)
}

fn manipulator(chr: Chr) -> Option<usize> {
    let ctrl = read_ptr(chr.0 + CHR_CTRL).filter(|&p| p >= 0x10000)?;
    read_ptr(ctrl + CTRL_MANIP)
        .filter(|&p| p >= 0x10000)
        .or_else(|| read_ptr(ctrl + CTRL_MANIP_ALT).filter(|&p| p >= 0x10000))
}

#[derive(Default)]
pub struct Motion {
    /// Sender: per state id, a counter bumped when the state is fresh, so a
    /// lost packet cannot hide a repeated attack.
    my_seq: HashMap<u16, u8>,
    /// Receiver: last counter seen per state id.
    their_seq: HashMap<u16, u8>,
    checked_ghost: Option<usize>,
    captured: u32,
    capture_fail: u32,
    applied: u32,
    stats_at: Option<Instant>,
}

impl Motion {
    /// Fills the motion part of our outgoing snapshot.
    pub fn capture(&mut self, player: Chr, snap: &mut Snapshot) {
        if let Some(m) = manipulator(player)
            && let Some(v) = read::<[f32; 4]>(m + MANIP_MOVE)
            && v.iter().all(|f| f.is_finite())
        {
            snap.move_vec = v;
        }
        if let Some(ar) = module(player, MOD_ACTION_REQUEST) {
            snap.dash = read::<u8>(ar + AR_DASH).is_some_and(|b| b & 4 != 0);
        }

        let Some(bm) = module(player, MOD_BEHAVIOR) else { return };
        let Some(heap) = read_ptr(module_base() + REPLAY_HEAP_RVA).filter(|&p| p >= 0x10000) else { return };
        let mut v = DlVec { alloc: heap, begin: 0, end: 0, cap: 0 };
        let query: extern "C" fn(usize, *mut DlVec) -> u8 =
            unsafe { std::mem::transmute(module_base() + QUERY_STATES_RVA) };
        query(bm + BM_SYNC_STATES, &mut v);
        let mut n = 0;
        if v.begin >= 0x10000 && v.end >= v.begin {
            let count = ((v.end - v.begin) / 4).min(MAX_SYNC_STATES);
            for i in 0..count {
                let Some(e) = read::<u32>(v.begin + 4 * i) else { break };
                // {u16 id, u8 fresh, u8 garbage}: the game leaves byte 3 unset.
                let id = (e & 0xFFFF) as u16;
                let fresh = (e >> 16) & 0xFF != 0;
                let seq = self.my_seq.entry(id).or_insert(0);
                if fresh {
                    *seq = seq.wrapping_add(1);
                }
                snap.states[n] = id as u32 | (*seq as u32) << 16;
                n += 1;
            }
            self.captured += 1;
        } else if v.begin != 0 {
            self.capture_fail += 1;
        }
        snap.n_states = n as u8;
        // Give the buffer back to the allocator it came from (vtable slot 13).
        if v.begin != 0
            && let Some(vt) = read_ptr(v.alloc)
            && let Some(free) = read_ptr(vt + 0x68)
        {
            let free: extern "C" fn(usize, usize) = unsafe { std::mem::transmute(free) };
            free(v.alloc, v.begin);
        }
    }

    /// Drives the ghost from the friend's (delayed) snapshot.
    pub fn apply(&mut self, ghost: Chr, s: &Snapshot) {
        let now = Instant::now();
        if self.checked_ghost != Some(ghost.0) {
            self.checked_ghost = Some(ghost.0);
            self.their_seq.clear();
            let f: extern "C" fn(usize) -> u8 = unsafe { std::mem::transmute(module_base() + IS_SYNC_DRIVEN_RVA) };
            log::line(&format!(
                "motion: new ghost, sync-driven check {} manipulator {:?}",
                f(ghost.0),
                manipulator(ghost).map(|p| format!("{p:#x}"))
            ));
        }

        // Walk/run: the movement vector, as the ReplayManipulator would write it.
        // Dash: 0xB4AFD0 sets MoveSpeedLevel = 2.0 when the action-request
        // dash bit is set and |v| > 0, otherwise level = |v| (unclamped). We
        // do not own the ghost's action-request flags, so carry the dash as
        // |v| = 2.0, which gives the same level.
        if let Some(m) = manipulator(ghost)
            && s.move_vec.iter().all(|f| f.is_finite() && f.abs() <= 2.0)
        {
            let mut v = s.move_vec;
            let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            if s.dash && len > 1e-4 {
                v = v.map(|f| f * (2.0 / len));
            }
            crate::game::write(m + MANIP_MOVE, v);
            crate::game::write(m + MANIP_MOVE_2, v);
            set_drive(ghost.0, v);
        }

        // States: fresh only when the friend's counter for it moved.
        let n = (s.n_states as usize).min(MAX_SYNC_STATES);
        let mut buf = [0u32; MAX_SYNC_STATES];
        for i in 0..n {
            let id = (s.states[i] & 0xFFFF) as u16;
            let seq = (s.states[i] >> 16) as u8;
            let fresh = self.their_seq.insert(id, seq).is_some_and(|old| old != seq);
            buf[i] = id as u32 | (fresh as u32) << 16;
        }
        if let Some(bsync) = module(ghost, MOD_BEHAVIOR_SYNC) {
            let begin = buf.as_ptr() as usize;
            let mut fake = DlVec { alloc: 0, begin, end: begin + 4 * n, cap: begin + 4 * n };
            let apply: extern "C" fn(usize, *mut DlVec, u32) =
                unsafe { std::mem::transmute(module_base() + APPLY_STATES_RVA) };
            apply(bsync, &mut fake, 0);
            self.applied += 1;
        }

        if self.stats_at.is_none_or(|t| now.duration_since(t) >= STATS_EVERY) {
            self.stats_at = Some(now);
            let bm = module(ghost, MOD_BEHAVIOR);
            // Sync ids below the dictionary's static count come from a fixed
            // table; ids above it are registered on demand (0x6EC680) and so
            // may differ between the two games. Log both counts to check live.
            let dict = read_ptr(module_base() + SYNC_DICT_RVA).and_then(|s| read_ptr(s + 0x10));
            log::line(&format!(
                "motion: captured {} (fail {}), applied {}; friend move {:?} dash {} states {:x?}; sync dict static {:?} total {:?}; ghost speed level {:?} angle {:?}",
                self.captured,
                self.capture_fail,
                self.applied,
                s.move_vec,
                s.dash,
                &s.states[..n],
                dict.and_then(|d| read::<i32>(d + 0xE4)),
                dict.and_then(|d| read::<i32>(d + 0xE0)),
                bm.and_then(|b| read::<f32>(b + BM_MOVE_SPEED_LEVEL)),
                bm.and_then(|b| read::<f32>(b + BM_MOVE_ANGLE)),
            ));
        }
    }
}

/// The ghost whose movement vector the hook at `0xB49D56` supplies, and the
/// vector (f32 bits). 0 = none.
static DRIVE_CHR: AtomicUsize = AtomicUsize::new(0);
static DRIVE_VEC: [AtomicU32; 4] = [const { AtomicU32::new(0) }; 4];
const CHR_OF_MODULE_RVA: usize = 0xB271D0;
const MOVE_FN_RVA: usize = 0xB4AFD0;

fn set_drive(ghost: usize, v: [f32; 4]) {
    for (slot, f) in DRIVE_VEC.iter().zip(v) {
        slot.store(f.to_bits(), Ordering::Relaxed);
    }
    DRIVE_CHR.store(ghost, Ordering::Release);
}

/// Replaces the call to `0xB4AFD0` (movement vector -> MoveSpeedLevel /
/// MoveAngle): writes the friend's vector into the ghost's manipulator
/// first. Everything else goes straight to the original.
pub extern "C" fn move_wrapper(module: usize) -> usize {
    let base = module_base();
    let ghost = DRIVE_CHR.load(Ordering::Acquire);
    if ghost != 0 {
        let chr_of: extern "C" fn(usize) -> usize = unsafe { std::mem::transmute(base + CHR_OF_MODULE_RVA) };
        if chr_of(module) == ghost
            && let Some(m) = manipulator(Chr(ghost))
        {
            let v = [
                f32::from_bits(DRIVE_VEC[0].load(Ordering::Relaxed)),
                f32::from_bits(DRIVE_VEC[1].load(Ordering::Relaxed)),
                f32::from_bits(DRIVE_VEC[2].load(Ordering::Relaxed)),
                f32::from_bits(DRIVE_VEC[3].load(Ordering::Relaxed)),
            ];
            crate::game::write(m + MANIP_MOVE, v);
            crate::game::write(m + MANIP_MOVE_2, v);
        }
    }
    let orig: extern "C" fn(usize) -> usize = unsafe { std::mem::transmute(base + MOVE_FN_RVA) };
    orig(module)
}
