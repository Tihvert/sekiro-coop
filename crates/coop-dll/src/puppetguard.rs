//! v8.60: the owner's type-5 HP does not kill a Puppeteer puppet here.
//!
//! A Puppeteer Ninjutsu deathblow kills the enemy on the machine that did it and the game
//! raises it as a puppet (`statelog::owner_team_state`: active SpEffect stateInfo 302 and
//! chr+0x70 == 2). On the other machine that enemy stays dead, so the enemy owner keeps
//! sending HP 0 for it in its type-5 table. v8.59 stopped livesync / persist from killing a
//! puppet; this is the remaining path (IDA 2026-10-28, `F:\Temp\re_puppet2` o1/o3, [VAR]):
//!
//! ```text
//! 0xA4B830 client branch, for every chr with a table entry (sentinel 0xF987 = none):
//!   hp_before = 0x5A9C80(data)                 // r15d
//!   0xBD64E0(data, entry.hp)                   // call at 0xA4BB88, returns to 0xA4BB8D
//!   0x9F6950(chr, entry.flags)
//!   if hp_before > 0 && 0xBD5D40(data)         // HP <= 0 and no SpEffect hold
//!       0x9EE7E0(chr, 0)                       // kill, remote = 0
//! ```
//!
//! The kill there is `remote = 0` (the `remote = 1` kills are NetworkManipulator `0x7EC250`,
//! remote players only, and the peer-damage path `0xA4F940`), and the HP is already 0 when it
//! runs, so blocking `0x9EE7E0` alone would leave a 0-HP puppet. Instead the HP setter
//! `0xBD64E0` is detoured and, only when it returns to `0xA4BB8D` (the type-5 apply) and the
//! chr is a puppet here, the write is skipped: the puppet keeps its own HP, `0xBD5D40` stays
//! false and the kill is never called. The chr comes from the data module's back pointer
//! `data+0x8` (`0xB271D0`, [VAR]). The caller address comes in r8 from the stub
//! (`hook::install_detour_caller_r8`), so no shared slot is raced by other threads.
//! Mode 4 (our own enemy sync, no native session) had the same hole: enemies.rs
//! `client_frame` wrote the host's HP 0 onto the puppet and drove it by the host's NetAI /
//! position; `mode4_hold` holds a puppet there like a rest-held enemy.
//! The host side needs nothing here: the host never applies type 5 (`0xA4B830` host branch
//! only writes the table), and v8.59 keeps livesync / persist off a host's puppet. [I] A
//! peer's damage packet (`0xA4F940`, kill `remote = 1`) is not guarded: the peer's copy is a
//! corpse and cannot hit the puppet.
//! Kill switch: `[GAMEPLAY] puppet_type5_guard = 0` (both paths).
use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use crate::{game, hook, log};

/// `[GAMEPLAY] puppet_type5_guard` (default on).
pub static ENABLED: AtomicBool = AtomicBool::new(true);

/// `0xBD64E0(data, hp)`: the HP setter (clamps to max `data+0x134`, keeps 1 while
/// `0xBD5DF0` says the chr cannot die).
const SET_HP_RVA: usize = 0xBD64E0;
/// mov [rsp+18h],rbx / mov [rsp+10h],edx / push rdi / sub rsp,20h (14 bytes, no RIP-relative operand).
const SET_HP_PROLOGUE: [u8; 14] = [0x48, 0x89, 0x5C, 0x24, 0x18, 0x89, 0x54, 0x24, 0x10, 0x57, 0x48, 0x83, 0xEC, 0x20];
/// The return address of the setter call in `0xA4B830`'s client (type-5 apply) branch.
const TYPE5_SET_HP_RET: usize = 0xA4BB8D;
/// `data+0x8` = the owning ChrIns (`0xB271D0` returns it).
const DATA_OWNER: usize = 0x8;
const DATA_HP: usize = 0x130;
/// `chr+0x78` = netId (the key `0xA4B830` looks the table up with).
const CHR_NET_ID: usize = 0x78;

static TRAMP: AtomicUsize = AtomicUsize::new(0);
static SKIPPED: AtomicU32 = AtomicU32::new(0);
/// Puppets logged (ChrIns address); cleared never: a few entries per session.
static LOGGED: Mutex<Option<HashSet<usize>>> = Mutex::new(None);

/// Skip this setter call? Only the type-5 apply, only for a puppet here.
pub fn skip_type5_hp(enabled: bool, caller_rva: usize, puppet: impl FnOnce() -> bool) -> bool {
    enabled && caller_rva == TYPE5_SET_HP_RET && puppet()
}

extern "C" fn set_hp_detour(data: usize, hp: i32, caller: usize) -> u64 {
    let caller_rva = caller.wrapping_sub(game::module_base());
    let mut puppet = None;
    if skip_type5_hp(ENABLED.load(Ordering::Relaxed), caller_rva, || {
        puppet = puppet_of(data);
        puppet.is_some()
    }) {
        SKIPPED.fetch_add(1, Ordering::Relaxed);
        if let Some(chr) = puppet {
            note(chr, data, hp);
        }
        return 0;
    }
    let f: extern "C" fn(usize, i32) -> u64 = unsafe { std::mem::transmute(TRAMP.load(Ordering::Acquire)) };
    f(data, hp)
}

/// The chr behind `data` when it is a puppet here.
fn puppet_of(data: usize) -> Option<usize> {
    let chr = game::read_ptr(data + DATA_OWNER)?;
    crate::statelog::owner_team_state(chr).filter(|&(_, on)| on).map(|_| chr)
}

/// Once per puppet: what the owner sent and what we kept.
fn note(chr: usize, data: usize, hp: i32) {
    let Ok(mut g) = LOGGED.lock() else { return };
    if !g.get_or_insert_with(HashSet::new).insert(chr) {
        return;
    }
    let entity = game::Chr(chr).entity_id().unwrap_or(-1);
    let net = game::read::<u16>(chr + CHR_NET_ID).unwrap_or(0xFFFF);
    let kept = game::read::<i32>(data + DATA_HP).unwrap_or(-1);
    let what = if hp <= 0 { "host's kill (HP 0) ignored".to_string() } else { format!("owner's HP {hp} ignored") };
    let n = SKIPPED.load(Ordering::Relaxed);
    log::line(&format!("type5: enemy {entity} netId {net:#x} is a puppet here: {what}, HP kept at {kept} ({n} type-5 writes skipped so far)"));
}

/// The host's state says dead, ours is alive: only then can the host's HP kill a puppet.
pub fn mode4_needs_check(local_hp: i32, host_hp: i32) -> bool {
    local_hp > 0 && host_hp <= 0
}

/// Mode 4 (our own enemy sync, enemies.rs `client_frame`): the host's HP 0 would be written
/// onto a puppet and its NetAI / position would follow the host's corpse. True = hold it
/// like a rest-held enemy. The SpEffect walk runs only when the host says dead and we do not.
pub fn mode4_hold(chr: usize, id: i32, local_hp: i32, host_hp: i32) -> bool {
    if !ENABLED.load(Ordering::Relaxed) || !mode4_needs_check(local_hp, host_hp) {
        return false;
    }
    if crate::statelog::owner_team_state(chr).is_none_or(|(_, on)| !on) {
        return false;
    }
    if let Ok(mut g) = LOGGED.lock()
        && g.get_or_insert_with(HashSet::new).insert(chr)
    {
        log::line(&format!("enemies: enemy {id} is a puppet here (HP {local_hp}), dead on the host: host's HP 0 and AI ignored"));
    }
    true
}

/// At start (title screen), like the other entry detours. The detour is installed even when
/// the guard is off in the settings file (it then passes every call through), so the switch
/// can change while the game runs.
pub fn install() {
    if TRAMP.load(Ordering::Acquire) != 0 {
        return;
    }
    let detour: extern "C" fn(usize, i32, usize) -> u64 = set_hp_detour;
    if let Some(t) = hook::install_detour_caller_r8(SET_HP_RVA, &SET_HP_PROLOGUE, detour as usize,"puppet type-5 HP guard (0xBD64E0)") {
        TRAMP.store(t, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_type5_apply_on_a_puppet_is_skipped() {
        assert!(skip_type5_hp(true, TYPE5_SET_HP_RET, || true));
        // Not a puppet: the owner's HP is applied as before.
        assert!(!skip_type5_hp(true, TYPE5_SET_HP_RET, || false));
        // Any other caller (our posture.rs set_hp, the game's own damage paths) passes.
        assert!(!skip_type5_hp(true, 0x9EE802, || true));
        assert!(!skip_type5_hp(true, 0, || true));
        // Kill switch.
        assert!(!skip_type5_hp(false, TYPE5_SET_HP_RET, || true));
    }

    #[test]
    fn mode4_checks_only_a_living_copy_the_host_calls_dead() {
        assert!(mode4_needs_check(120, 0));
        assert!(mode4_needs_check(1, -5));
        assert!(!mode4_needs_check(0, 0), "dead here too: the normal corpse path");
        assert!(!mode4_needs_check(120, 300), "alive on the host: nothing to block");
    }

    #[test]
    fn the_puppet_check_runs_only_for_the_type5_caller() {
        let mut asked = false;
        assert!(!skip_type5_hp(true, 0x123, || {
            asked = true;
            true
        }));
        assert!(!asked, "the SpEffect walk must not run on every HP write");
    }
}
