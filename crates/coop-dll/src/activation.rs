//! Host: enemies near ANY peer get full AI updates too (RE-ENEMY-TARGETING.md).
//!
//! N players: puppet.rs passes every living peer in our block (`set_peers`, up to
//! `MAX_PEERS`). Off while the native session owns the enemies (nsession `step_aside`:
//! then every peer in our block is engaged, nsession's own-sync rule, and each owns the
//! enemies near it).
//!
//! The game sets every character's update interval (`chr+0x110`: 0 = every
//! frame, 2/5/30 = every n-th, -1 = off) only from the distance to the host's
//! main player (`0xA4E8E0`, all writes through the setter `0x9F6270`). Enemies
//! near the friend but far from the host therefore think every 5 or 30 frames
//! and barely react to the friend. We replace the small setter with an exact
//! copy that forces 0 for up to `MAX_FORCED` enemies within `RADIUS_M` of the
//! friend. -1 (map entity passive) and -2 are left alone and only counted:
//! if they show up near the friend, this is not enough (see the report).

use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::game::{module_base, read, read_ptr};
use crate::log;
use crate::peers::MAX_PEERS;

const SETTER_RVA: usize = 0x9F6270;
const SETTER_PROLOGUE: [u8; 7] = [0xF6, 0x81, 0x10, 0x1A, 0x00, 0x00, 0x01];
const ENEMY_VTABLE_RVA: usize = 0x2A27F28;
const RADIUS_M: f32 = 40.0;
const MAX_FORCED: u32 = 16;

/// How many entries of `PEER_POS` are live (0 = no peer: the rule is off).
static PEER_COUNT: AtomicUsize = AtomicUsize::new(0);
static PEER_POS: [[AtomicU32; 3]; MAX_PEERS] = [const { [const { AtomicU32::new(0) }; 3] }; MAX_PEERS];
static FRAME: AtomicU64 = AtomicU64::new(0);
static SEEN_FRAME: AtomicU64 = AtomicU64::new(u64::MAX);
static FORCED_THIS_FRAME: AtomicU32 = AtomicU32::new(0);
// Stats since the last log (enemies within RADIUS_M of the friend).
static NEAR_CALLS: AtomicU32 = AtomicU32::new(0);
static NEAR_FORCED: AtomicU32 = AtomicU32::new(0);
static NEAR_ALREADY_0: AtomicU32 = AtomicU32::new(0);
static NEAR_OFF: AtomicU32 = AtomicU32::new(0);
static NEAR_OVER_BUDGET: AtomicU32 = AtomicU32::new(0);

pub fn install() -> bool {
    crate::hook::redirect_function(SETTER_RVA, &SETTER_PROLOGUE, set_interval as *const () as usize, "update-interval setter")
}

/// Host, every frame: the positions of the peers the rule serves (living, in our block;
/// empty = none). At most `MAX_PEERS` are used.
pub fn set_peers(pos: &[[f32; 3]]) {
    FRAME.fetch_add(1, Ordering::Relaxed);
    let n = pos.len().min(MAX_PEERS);
    for (slot, p) in PEER_POS.iter().zip(&pos[..n]) {
        for (a, f) in slot.iter().zip(p) {
            a.store(f.to_bits(), Ordering::Relaxed);
        }
    }
    PEER_COUNT.store(n, Ordering::Release);
}

/// Native remote player chrs forced to every-frame updates (0 = empty slot).
static REMOTES: [AtomicUsize; MAX_PEERS] = [const { AtomicUsize::new(0) }; MAX_PEERS];

/// Every frame: the native remotes to force (empty = none; set while we spectate).
pub fn set_forced_remotes(chrs: &[usize]) {
    for (i, slot) in REMOTES.iter().enumerate() {
        slot.store(chrs.get(i).copied().unwrap_or(0), Ordering::Relaxed);
    }
}

/// Some peer stands within `RADIUS_M` of `p`.
fn near_a_peer(p: [f32; 3], count: usize) -> bool {
    PEER_POS[..count.min(MAX_PEERS)].iter().any(|slot| {
        let f = slot.each_ref().map(|a| f32::from_bits(a.load(Ordering::Relaxed)));
        crate::game::distance(p, f) < RADIUS_M
    })
}

/// Exact copy of `0x9F6270(chr, n)` plus the friend rule. Runs on the game
/// The chr's position with plain loads (modules `+0x1FF8`, physics `+0x68`, position
/// `+0x80`). `Chr::position` probes each pointer with a ReadProcessMemory syscall:
/// three syscalls per enemy per frame here cost the host frames (live 2026-10-23).
/// Same trust as the original setter's own unguarded loads (the game's live chr).
fn position_fast(chr: usize) -> Option<[f32; 3]> {
    unsafe {
        let modules = *((chr + 0x1FF8) as *const usize);
        if modules < 0x10000 {
            return None;
        }
        let physics = *((modules + 0x68) as *const usize);
        if physics < 0x10000 {
            return None;
        }
        Some(*((physics + 0x80) as *const [f32; 3]))
    }
}

/// thread inside `0xA4E8E0`; must never panic.
extern "C" fn set_interval(chr: usize, n: i32) {
    // The original reads these unguarded too (chr is the game's own live object),
    // so plain loads here: this runs for every character every frame.
    let mut n = n;
    unsafe {
        if *((chr + 0x1A10) as *const u8) & 1 != 0 {
            return;
        }
        let m = *(*((chr + 0x1FF8) as *const usize) as *const usize);
        let flag = m != 0 && (*((m + 0x10) as *const u32) >> 25) & 1 != 0;
        if n == 30 && flag {
            n = 5;
        }
    }
    // Spectating: the watched players' native remotes run every frame too (v8.6: the
    // friend held one pose for 3-5 frames on the spectator camera, live 2026-10-25).
    if n > 0 && REMOTES.iter().any(|r| r.load(Ordering::Relaxed) == chr) {
        n = 0;
    }
    let count = PEER_COUNT.load(Ordering::Acquire);
    let is_enemy = count > 0 && unsafe { *(chr as *const usize) } == module_base() + ENEMY_VTABLE_RVA;
    if is_enemy
        && let Some(p) = position_fast(chr)
        && near_a_peer(p, count)
    {
        NEAR_CALLS.fetch_add(1, Ordering::Relaxed);
        if n < 0 {
            NEAR_OFF.fetch_add(1, Ordering::Relaxed);
        } else if n == 0 {
            NEAR_ALREADY_0.fetch_add(1, Ordering::Relaxed);
        } else {
            let frame = FRAME.load(Ordering::Relaxed);
            if SEEN_FRAME.swap(frame, Ordering::Relaxed) != frame {
                FORCED_THIS_FRAME.store(0, Ordering::Relaxed);
            }
            if FORCED_THIS_FRAME.fetch_add(1, Ordering::Relaxed) < MAX_FORCED {
                n = 0;
                NEAR_FORCED.fetch_add(1, Ordering::Relaxed);
            } else {
                NEAR_OVER_BUDGET.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    unsafe { *((chr + 0x110) as *mut i32) = n };
}

/// Host, every few seconds: what the rule did, plus each native remote's chr
/// fields the enemy target search needs (`+0x1A15` bits 0 and 7, entity
/// `*(chr+0x18)`: `+0x10 == 4`, `+0x20 & 7 == 0`, `+0x30 == 0`).
pub fn log_stats(remotes: &[usize]) {
    let take = |a: &AtomicU32| a.swap(0, Ordering::Relaxed);
    let mut line = format!(
        "activation: enemies near the friend: forced to every-frame {}, already every-frame {}, off (-1/-2, not touched) {}, over budget {} (calls {})",
        take(&NEAR_FORCED),
        take(&NEAR_ALREADY_0),
        take(&NEAR_OFF),
        take(&NEAR_OVER_BUDGET),
        take(&NEAR_CALLS)
    );
    for &r in remotes {
        let ent = read_ptr(r + 0x18);
        line.push_str(&format!(
            "; remote interval +0x110 {:?} +0x1A15 {:?} entity state {:?} flags {:?} +0x30 {:?} team {:?}",
            read::<i32>(r + 0x110),
            read::<u8>(r + 0x1A15).map(|b| format!("{b:#04x}")),
            ent.and_then(|e| read::<u32>(e + 0x10)),
            ent.and_then(|e| read::<u8>(e + 0x20)).map(|b| format!("{b:#04x}")),
            ent.and_then(|e| read::<usize>(e + 0x30)).map(|p| format!("{p:#x}")),
            read::<u32>(r + 0x74)
        ));
    }
    log::line(&line);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_covers_every_peer_position() {
        set_peers(&[[0.0, 0.0, 0.0], [100.0, 0.0, 0.0]]);
        let n = PEER_COUNT.load(Ordering::Acquire);
        assert_eq!(n, 2);
        assert!(near_a_peer([10.0, 0.0, 0.0], n));
        assert!(near_a_peer([90.0, 0.0, 0.0], n));
        assert!(!near_a_peer([50.0, 0.0, 0.0], n));
        // More than MAX_PEERS: the rest are ignored.
        set_peers(&[[0.0; 3]; MAX_PEERS + 2]);
        assert_eq!(PEER_COUNT.load(Ordering::Acquire), MAX_PEERS);
        set_peers(&[]);
        assert_eq!(PEER_COUNT.load(Ordering::Acquire), 0);
    }
}
