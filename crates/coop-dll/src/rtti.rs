//! RE helper (F11): name C++ objects by their MSVC RTTI.
//!
//! For an object pointer: vftable = *obj; CompleteObjectLocator = vftable[-1];
//! COL.signature == 1 (x64), COL+0xC = TypeDescriptor RVA, name at TD+0x10
//! (e.g. ".?AVWorldChrManImp@CS@@"). We walk the WorldChrMan and
//! WorldChrManDbg singletons (RVAs from SekiroTool) and log every member that
//! is an object with RTTI, plus one level below WorldChrManDbg.

// `dump` (full RTTI walk) is kept for later RE; F11 now dumps the creator.
#![allow(dead_code)]

use crate::game::{module_base, read, read_ptr};
use crate::log;

const ROOTS: [(&str, usize, usize); 2] = [
    ("WorldChrMan", 0x3D7A1E0, 0x4000),
    ("WorldChrManDbg", 0x3D7A388, 0x400),
];

/// Our own vtable copies (ghost solid look, NetAI enemies). They live on the
/// heap, outside the exe: without this list class_name() rejected the solid
/// ghost, the mod forgot it and spawned a new one every 5 s (test 2026-10-22,
/// several green ghosts at once).
static VTABLE_COPIES: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

pub fn register_vtable_copy(vft: usize) {
    if let Ok(mut v) = VTABLE_COPIES.lock()
        && !v.contains(&vft)
    {
        v.push(vft);
    }
}

fn is_vtable_copy(vft: usize) -> bool {
    VTABLE_COPIES.lock().is_ok_and(|v| v.contains(&vft))
}

pub fn class_name(obj: usize) -> Option<String> {
    let base = module_base();
    let vft = read_ptr(obj)?;
    // vftables live inside the exe image; this filters most non-objects early.
    if (vft < base || vft > base + 0x5000_0000) && !is_vtable_copy(vft) {
        return None;
    }
    let col = read_ptr(vft - 8)?;
    if read::<u32>(col)? != 1 {
        return None;
    }
    let td = base + read::<u32>(col + 0xC)? as usize;
    // One bulk read instead of one per byte (TypeDescriptors are in the exe's
    // .data, so 64 bytes past the name start are always mapped).
    let mut buf = [0u8; 64];
    if !crate::game::read_bytes(td + 0x10, &mut buf) {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let name: String = buf[..end].iter().map(|&b| b as char).collect();
    name.starts_with(".?A").then_some(name)
}

fn dump_members(label: &str, obj: usize, len: usize, depth: u32) {
    for off in (0..len).step_by(8) {
        let Some(p) = read_ptr(obj + off) else { continue };
        if let Some(name) = class_name(p) {
            log::line(&format!("rtti: {label}+{off:#x} -> {p:#x} {name}"));
            if depth > 0 {
                dump_members(&format!("{label}+{off:#x}"), p, 0x200, depth - 1);
            }
        }
    }
}

/// `WorldChrMan + 0x3148 -> CSDebugChrCreator` (found by the RTTI dump, 2026-10-22).
const WCM_DEBUG_CHR_CREATOR: usize = 0x3148;
const WORLD_CHR_MAN_RVA: usize = 0x3D7A1E0;

/// F11: hex/float/utf16 dump of CSDebugChrCreator to learn its field layout
/// (ER's layout: spawn bool ~+0x44, init_data ~+0xB0 with position, param ids,
/// a UTF-16 "cXXXX" name).
pub fn dump_debug_chr_creator() {
    let Some(wcm) = read_ptr(module_base() + WORLD_CHR_MAN_RVA) else {
        log::line("creator: WorldChrMan null");
        return;
    };
    let Some(cc) = read_ptr(wcm + WCM_DEBUG_CHR_CREATOR) else {
        log::line("creator: pointer null");
        return;
    };
    log::line(&format!("creator: CSDebugChrCreator at {cc:#x} {:?}", class_name(cc)));
    for row in (0..0x300).step_by(16) {
        let Some(bytes) = read::<[u8; 16]>(cc + row) else { break };
        let u32s: Vec<String> = bytes
            .chunks(4)
            .map(|c| format!("{:08x}", u32::from_le_bytes([c[0], c[1], c[2], c[3]])))
            .collect();
        let f32s: Vec<String> = bytes
            .chunks(4)
            .map(|c| {
                let f = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                if f.is_finite() && f.abs() > 1e-3 && f.abs() < 1e5 { format!("{f:.2}") } else { "-".into() }
            })
            .collect();
        let text: String = bytes
            .chunks(2)
            .map(|c| match u16::from_le_bytes([c[0], c[1]]) {
                v @ 0x20..=0x7e => v as u8 as char,
                _ => '.',
            })
            .collect();
        let named = read_ptr(cc + row).and_then(class_name).unwrap_or_default();
        log::line(&format!(
            "creator: +{row:03x} {} | {} | {text} {named}",
            u32s.join(" "),
            f32s.join(" ")
        ));
    }
}

// CSDebugChrCreator layout, from IDA on the decrypted 1.06 dump (2026-10-22,
// re/out2): ctor sub_140C0EA70, steps at 0x143D88050 = Idle / Load / LoadWait.
//   +0x40 u32  current step      +0x44 u32 requested next step (vft04)
//   +0x100     init data passed to WorldChrMan create (sub_140A492E0):
//     +0x100 [f32;4] position (w = 1)
//     +0x114 u32  load param 2     +0x118 u32 chr id for model load (0 = c0000?)
//     +0x120 [u16;32] name buffer (inline), +0x168 ptr to it, +0x178 len, +0x180 cap (31)
//   +0x1AC f32 180.0 (rotation, degrees?)   +0x1B0 u8 "idle" (Idle step sets 1)
//   +0x1B8 ChrIns* last created character (set by LoadWait)
const CC_STEP: usize = 0x40;
const CC_NEXT_STEP: usize = 0x44;
const CC_POS: usize = 0x100;
const CC_CHR_ID: usize = 0x118;
const CC_NAME_BUF: usize = 0x120;
const CC_NAME_PTR: usize = 0x168;
const CC_NAME_LEN: usize = 0x178;
const CC_NAME_CAP: usize = 0x180;
const CC_IDLE: usize = 0x1B0;
const CC_LAST_CREATED: usize = 0x1B8;
const STEP_LOAD: u32 = 1;

fn debug_chr_creator() -> Option<usize> {
    read_ptr(read_ptr(module_base() + WORLD_CHR_MAN_RVA)? + WCM_DEBUG_CHR_CREATOR)
}

pub struct SpawnRequest {
    pub creator: usize,
    pub before: Option<usize>,
}

/// F12: fill the creator's init data and request STEP_Load; the game's own
/// state machine then loads the model and creates the character.
pub fn request_spawn(chr_name: &str, chr_id: u32, pos: [f32; 3]) -> Result<SpawnRequest, String> {
    let cc = debug_chr_creator().ok_or("creator pointer null")?;
    let cap = read::<u64>(cc + CC_NAME_CAP).ok_or("cap unreadable")?;
    let ptr = read::<usize>(cc + CC_NAME_PTR).ok_or("name ptr unreadable")?;
    let step = read::<u32>(cc + CC_STEP).ok_or("step unreadable")?;
    let idle = read::<u8>(cc + CC_IDLE);
    log::line(&format!(
        "spawn: creator {cc:#x} step {step} idle {idle:?} name cap {cap} ptr {ptr:#x} (expect {:#x}) chr id {:?} last {:?}",
        cc + CC_NAME_BUF,
        read::<u32>(cc + CC_CHR_ID),
        read::<usize>(cc + CC_LAST_CREATED)
    ));
    // Only write if the layout matches what IDA showed (inline name buffer).
    if cap != 31 || ptr != cc + CC_NAME_BUF {
        return Err(format!("layout mismatch (cap {cap}, ptr {ptr:#x})"));
    }
    if step != 0 {
        return Err(format!("creator busy (step {step})"));
    }
    let units: Vec<u16> = chr_name.encode_utf16().take(31).collect();
    let mut buf = [0u16; 32];
    buf[..units.len()].copy_from_slice(&units);
    let before = read::<usize>(cc + CC_LAST_CREATED);
    let ok = crate::game::write(cc + CC_POS, [pos[0], pos[1], pos[2], 1.0f32])
        && crate::game::write(cc + CC_NAME_BUF, buf)
        && crate::game::write(cc + CC_NAME_LEN, units.len() as u64)
        && crate::game::write(cc + CC_CHR_ID, chr_id)
        && crate::game::write(cc + CC_NEXT_STEP, STEP_LOAD);
    if !ok {
        return Err("write failed".into());
    }
    Ok(SpawnRequest { creator: cc, before })
}

/// Logs the state machine's progress and the created ChrIns, if any.
pub fn watch_spawn(r: &SpawnRequest, frame: u32) -> Option<usize> {
    let cc = r.creator;
    let last = read::<usize>(cc + CC_LAST_CREATED);
    let created = last.filter(|&p| p >= 0x10000 && Some(p) != r.before);
    log::line(&format!(
        "spawn: frame {frame} step {:?} next {:?} idle {:?} last {:?} {}",
        read::<u32>(cc + CC_STEP),
        read::<u32>(cc + CC_NEXT_STEP),
        read::<u8>(cc + CC_IDLE),
        last.map(|p| format!("{p:#x}")),
        created.and_then(class_name).unwrap_or_default()
    ));
    created
}

pub fn dump() {
    let base = module_base();
    for (label, rva, len) in ROOTS {
        let Some(obj) = read_ptr(base + rva) else {
            log::line(&format!("rtti: {label} global null"));
            continue;
        };
        log::line(&format!("rtti: {label} = {obj:#x} {:?}", class_name(obj)));
        let depth = if label == "WorldChrManDbg" { 1 } else { 0 };
        dump_members(label, obj, len, depth);
    }
    log::line("rtti: done");
}
