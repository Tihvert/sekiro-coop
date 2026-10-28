//! Player-model puppet: a ReplayGhostIns (the 1.06 "Remnant" ghost class, a
//! PlayerIns subclass with the player's ChrAsm body) created by the game's own
//! factory with an empty replay, so nothing but us moves it.
//!
//! From IDA on the decrypted 1.06 dump (re/out5-7, 2026-10-22):
//!   sub_1419DE730(size, align, heap)        game allocator
//!   qword_143D9CE50                          heap used for replay data (488 B)
//!   sub_1409A7F70(mem)                       replay data ctor (+168 frame count = 0,
//!                                            +176 frames = null, +212 FACE data defaults)
//!   sub_140A494A0(WorldChrMan, type, data)   creates ReplayGhostIns in WCM+0x58 set
//!                                            (bloodstains use type 3; type 10 counts
//!                                            something extra; 3 and 11 set +0x22A1)
//! With zero frames the ReplayManipulator plays nothing (it checks +168 > 0),
//! so position comes from our UpdateCoords hook like any other puppet.

use crate::game::{Chr, module_base, read_ptr};
use crate::log;
use crate::rtti;

const ALLOC_RVA: usize = 0x19DE730;
const REPLAY_HEAP_RVA: usize = 0x3D9CE50;
const REPLAY_DATA_CTOR_RVA: usize = 0x9A7F70;
const CREATE_REPLAY_GHOST_RVA: usize = 0xA494A0;
const WORLD_CHR_MAN_RVA: usize = 0x3D7A1E0;
const REPLAY_DATA_SIZE: usize = 488;

pub fn spawn(ghost_type: u32) -> Result<Chr, String> {
    let base = module_base();
    let wcm = read_ptr(base + WORLD_CHR_MAN_RVA).ok_or("WorldChrMan null")?;
    let heap = read_ptr(base + REPLAY_HEAP_RVA).ok_or("replay heap null")?;

    let alloc: extern "C" fn(usize, usize, usize) -> usize = unsafe { std::mem::transmute(base + ALLOC_RVA) };
    let ctor: extern "C" fn(usize) -> usize = unsafe { std::mem::transmute(base + REPLAY_DATA_CTOR_RVA) };
    let create: extern "C" fn(usize, u32, usize) -> usize =
        unsafe { std::mem::transmute(base + CREATE_REPLAY_GHOST_RVA) };

    let mem = alloc(REPLAY_DATA_SIZE, 8, heap);
    if mem < 0x10000 {
        return Err("allocation failed".into());
    }
    let data = ctor(mem);
    log::line(&format!("ghost: replay data {data:#x} (heap {heap:#x}), creating type {ghost_type}"));
    let p = create(wcm, ghost_type, data);
    if p < 0x10000 {
        return Err("CreateReplayGhost returned null (ghost set full?)".into());
    }
    let chr = Chr(p);
    log::line(&format!(
        "ghost: CREATED {p:#x} {:?} chr id {:?} pos {:?} hp {:?}",
        rtti::class_name(p),
        chr.character_id(),
        chr.position(),
        chr.hp()
    ));
    Ok(chr)
}

/// sub_140A493C0(WorldChrMan): ReplayGhostIns type 3 WITHOUT replay data
/// (second ctor sub_140A39280), then sets +0x1A3C = 1. The data-backed ghost
/// above was deleted on the next frame (empty replay = finished), test
/// 2026-10-22; this one has no replay that could finish.
const CREATE_PLAIN_GHOST_RVA: usize = 0xA493C0;

pub fn spawn_plain() -> Result<Chr, String> {
    let base = module_base();
    let wcm = read_ptr(base + WORLD_CHR_MAN_RVA).ok_or("WorldChrMan null")?;
    let create: extern "C" fn(usize) -> usize = unsafe { std::mem::transmute(base + CREATE_PLAIN_GHOST_RVA) };
    let p = create(wcm);
    if p < 0x10000 {
        return Err("CreatePlainGhost returned null (ghost set full?)".into());
    }
    let chr = Chr(p);
    log::line(&format!(
        "ghost: CREATED plain {p:#x} {:?} chr id {:?} pos {:?} hp {:?}",
        rtti::class_name(p),
        chr.character_id(),
        chr.position(),
        chr.hp()
    ));
    Ok(chr)
}

/// Equipment holder ("PG" in asserts) of a PlayerIns / ReplayGhostIns:
/// `ChrIns + 0x2140` (vftable slot 94 returns it). Equip ids live at
/// `holder + 0x24 + 4*slot` (slot < 25). Setting one goes through
/// sub_1409CCBA0(holder, slot, &id), which also loads/unloads the part model
/// (refcount in sub_140C398B0). IDA re/out8-9, 2026-10-22: a ghost with no
/// equipment had no visible body.
const EQUIP_HOLDER: usize = 0x2140;
const EQUIP_IDS: usize = 0x24;
const EQUIP_SLOTS: usize = 25;
const SET_EQUIP_RVA: usize = 0x9CCBA0;

/// Copies the local player's equipment onto `ghost` so it looks like the player.
pub fn dress_like(ghost: Chr, player: Chr) -> Result<usize, String> {
    let gh = read_ptr(ghost.0 + EQUIP_HOLDER).ok_or("ghost equip holder null")?;
    let ph = read_ptr(player.0 + EQUIP_HOLDER).ok_or("player equip holder null")?;
    let set: extern "C" fn(usize, i32, *const i32) -> usize =
        unsafe { std::mem::transmute(module_base() + SET_EQUIP_RVA) };
    let mut changed = 0;
    let mut ids = Vec::new();
    for slot in 0..EQUIP_SLOTS {
        let Some(id) = crate::game::read::<i32>(ph + EQUIP_IDS + 4 * slot) else { continue };
        let cur = crate::game::read::<i32>(gh + EQUIP_IDS + 4 * slot);
        ids.push(id);
        if cur != Some(id) {
            set(gh, slot as i32, &id);
            changed += 1;
        }
    }
    log::line(&format!("ghost: dressed like player, {changed} slots changed, player ids {ids:?}"));
    Ok(changed)
}

/// ReplayGhostIns `+0x22A1`: set by the ctor for ghost types 3 and 11
/// (`((type - 3) & ~8) == 0`). Bloodstain/Remnant ghosts are see-through; our
/// assumption is that this byte selects that look. 0 = try a solid body.
const GHOST_TRANSLUCENT_FLAG: usize = 0x22A1;

pub fn set_translucent(ghost: Chr, on: bool) -> bool {
    crate::game::write(ghost.0 + GHOST_TRANSLUCENT_FLAG, on as u8)
}

/// `+0x1A3C` (dword 1679): set to 1 by the data-less factory sub_140A057A0.
/// The replay-data ghost, which does not get it, was deleted by the game on
/// the next frame, so we read it as "keep alive". Clearing it should make the
/// game remove the ghost itself (assumption, logged for verification).
const GHOST_KEEP_ALIVE: usize = 0x1A3C;

pub fn despawn(ghost: Chr) -> bool {
    let ok = crate::game::write(ghost.0 + GHOST_KEEP_ALIVE, 0u32);
    log::line(&format!("ghost: despawn requested for {:#x} (ok = {ok})", ghost.0));
    ok
}


// --- Solid (non-phantom) look, RE-FINDINGS-2 § Ghost appearance -----------
//
// The phantom shading comes from ReplayGhostIns vtable slot 248 ("is replay
// ghost", always true for the ghost; PlayerIns' version returns false) and
// chr type 3; both seed the model's draw flags when the model is built, and
// slot 248 also makes the game write a ghost shader value (-0.5) every frame.

const GHOST_VTABLE_RVA: usize = 0x2A2D190;
const VT_SLOTS: usize = 253;
const SLOT_IS_REPLAY_GHOST: usize = 248;
const PLAYER_IS_REPLAY_GHOST_RVA: usize = 0xA2B5B0;
const ASM_MODEL: usize = 0x2150;
const ASM_GHOST_SHADER: usize = 0xD8;
const ASM_DIRTY: usize = 0x108;
const SET_CHR_TYPE_RVA: usize = 0x9F5170;
const CHR_TYPE_OTHER_PLAYER: i32 = 8;

/// Gives `ghost` its own copy of the ReplayGhostIns vtable with slot 248
/// answering "not a replay ghost". Must run right after the factory, before
/// the model is built. The copy is leaked on purpose (2 KB per ghost): the
/// game may call through it until the object is gone.
pub fn make_solid(ghost: Chr) -> Result<(), &'static str> {
    let base = module_base();
    let vt = base + GHOST_VTABLE_RVA;
    if read_ptr(ghost.0) != Some(vt) {
        return Err("not the ReplayGhostIns vtable (already changed?)");
    }
    let mut copy: Vec<usize> = Vec::with_capacity(VT_SLOTS + 1);
    // [-1] = RTTI locator, so class_name() still works.
    for i in 0..=VT_SLOTS {
        copy.push(read_ptr(vt - 8 + 8 * i).ok_or("vtable unreadable")?);
    }
    copy[1 + SLOT_IS_REPLAY_GHOST] = base + PLAYER_IS_REPLAY_GHOST_RVA;
    // The phantom colours are the PhantomParam row slot 130 returns, applied
    // every frame (RE-FINDINGS-2 § Solid ghost (2)): answer with the local
    // player's row, i.e. no phantom.
    copy[1 + SLOT_PHANTOM_ROW] = ghost_phantom_row as *const () as usize;
    let copy: &'static mut [usize] = copy.leak();
    let new_vt = copy.as_ptr() as usize + 8;
    rtti::register_vtable_copy(new_vt);
    if !crate::game::write(ghost.0, new_vt) {
        return Err("vtable write failed");
    }
    log::line(&format!("ghost: solid look (own vtable {new_vt:#x})"));
    Ok(())
}

const SLOT_PHANTOM_ROW: usize = 130;

/// Our vtable slot 130 for the ghost: the local player's PhantomParam row
/// (normally none), or -1 (none) without a player. Called by the game's draw
/// sync on the game thread.
pub(crate) extern "C" fn ghost_phantom_row(_chr: usize) -> i32 {
    let Some(p) = crate::game::player() else { return -1 };
    let Some(f) = read_ptr(p.0).and_then(|vt| read_ptr(vt + 8 * SLOT_PHANTOM_ROW)) else { return -1 };
    let f: extern "C" fn(usize) -> i32 = unsafe { std::mem::transmute(f) };
    f(p.0)
}

/// For the log: the row the ghost and the player report.
pub fn log_phantom_rows(ghost: Chr, player: Chr) {
    let row = |c: Chr| -> Option<i32> {
        let f = read_ptr(read_ptr(c.0)? + 8 * SLOT_PHANTOM_ROW)?;
        let f: extern "C" fn(usize) -> i32 = unsafe { std::mem::transmute(f) };
        Some(f(c.0))
    };
    let draw_group = read_ptr(ghost.0 + 0x48).and_then(|m| crate::game::read::<u8>(m + 0x23));
    log::line(&format!(
        "ghost: phantom row ghost {:?} player {:?}, model draw group {draw_group:?} (7 = clone in time, 8 = too late)",
        row(ghost),
        row(player)
    ));
}

/// Every frame: clear the ghost shader value (game default 0.0) and mark the
/// model dirty. With `make_solid` the game stops writing -0.5 itself.
pub fn solid_shader_tick(ghost: Chr) {
    let Some(asm) = read_ptr(ghost.0 + ASM_MODEL).filter(|&p| p >= 0x10000) else { return };
    if crate::game::read::<f32>(asm + ASM_GHOST_SHADER).is_some_and(|v| v != 0.0) {
        crate::game::write(asm + ASM_GHOST_SHADER, 0.0f32);
        if let Some(d) = crate::game::read::<u64>(asm + ASM_DIRTY) {
            crate::game::write(asm + ASM_DIRTY, d | 0x400);
        }
    }
}

/// Chr type 8 (another player, not a ghost) and re-dress so the parts reload
/// without the ghost flag. Riskier: other code keyed on the type changes too.
pub fn set_other_player_type(ghost: Chr, player: Chr) {
    let f: extern "C" fn(usize, i32) = unsafe { std::mem::transmute(module_base() + SET_CHR_TYPE_RVA) };
    f(ghost.0, CHR_TYPE_OTHER_PLAYER);
    let r = dress_like(ghost, player);
    log::line(&format!("ghost: chr type set to {CHR_TYPE_OTHER_PLAYER}, re-dressed ({r:?})"));
}
