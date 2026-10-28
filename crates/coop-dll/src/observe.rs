//! Read-only observation of the game's own netcode (test builds, feature
//! `testsync`; docs/THEMIDA-ANALYSIS.md §4.1). Nothing here changes what the
//! game does: each hook counts the call (and its caller and first arguments)
//! and then runs the original instructions.
//!
//! Hooked (entry, prologue relocated; addresses and roles from
//! RE-FINDINGS-2.md and RE-SEKIRO-ONLINE.md):
//! - `0x8E7210(mgr, type, data, size)` FrpgSessionManager broadcast send;
//!   `0x8E6B50(mgr, member, type, ptr, cap)` receive.
//! - Session queries `0x1072F80` (IsHost, S+0x184 == 3), `0x1072F00`
//!   (IsClient, == 6), `0x8E4B10` (FSM+0xDC == 4).
//! - NetAI sync-table methods: `0xA1C100` send type 4, `0xA1B320` send 70,
//!   `0xA1DAC0` send 71, `0xA1CED0` send 22, `0xA1DD00` owner list 23,
//!   `0xA1BDE0` recv 4, `0xA1AF40` recv 70, `0xA1C860` recv 5.
//! The getters whose prologue is RIP-relative (InSession, member count,
//! offline flag) are read as memory every log instead.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::game::{module_base, read, read_ptr};
use crate::log;

struct Target {
    rva: usize,
    prologue: &'static [u8],
    name: &'static str,
    /// Which captured argument is the packet type: 0 none, 2 = rdx, 3 = r8.
    type_arg: u8,
}

const TARGETS: &[Target] = &[
    Target { rva: 0x8E7210, prologue: &[0x48, 0x89, 0x5C, 0x24, 0x10], name: "broadcast send", type_arg: 2 },
    Target { rva: 0x8E6B50, prologue: &[0x48, 0x89, 0x5C, 0x24, 0x08], name: "member recv", type_arg: 3 },
    Target { rva: 0x1072F80, prologue: &[0x83, 0xB9, 0x84, 0x01, 0x00, 0x00, 0x03], name: "IsHost", type_arg: 0 },
    Target { rva: 0x1072F00, prologue: &[0x83, 0xB9, 0x84, 0x01, 0x00, 0x00, 0x06], name: "IsClient", type_arg: 0 },
    Target { rva: 0x8E4B10, prologue: &[0x83, 0xB9, 0xDC, 0x00, 0x00, 0x00, 0x04], name: "FSM client", type_arg: 0 },
    Target { rva: 0xA1C100, prologue: &[0x48, 0x8B, 0xC4, 0x55, 0x41, 0x54], name: "netai send 4 (move)", type_arg: 0 },
    Target { rva: 0xA1B320, prologue: &[0x40, 0x56, 0x48, 0x83, 0xEC, 0x50], name: "netai send 70 (behavior)", type_arg: 0 },
    Target { rva: 0xA1DAC0, prologue: &[0x40, 0x56, 0x48, 0x83, 0xEC, 0x40], name: "netai send 71 (action)", type_arg: 0 },
    Target { rva: 0xA1CED0, prologue: &[0x40, 0x56, 0x48, 0x83, 0xEC, 0x30], name: "netai send 22 (candidates)", type_arg: 0 },
    Target { rva: 0xA1DD00, prologue: &[0x40, 0x55, 0x56, 0x57, 0x41, 0x54], name: "netai owner list 23", type_arg: 0 },
    Target { rva: 0xA1BDE0, prologue: &[0x40, 0x55, 0x56, 0x48, 0x81, 0xEC, 0xA8, 0x00, 0x00, 0x00], name: "netai recv 4", type_arg: 0 },
    Target { rva: 0xA1AF40, prologue: &[0x40, 0x53, 0x41, 0x57, 0x48, 0x81, 0xEC, 0xE8, 0x01, 0x00, 0x00], name: "netai recv 70", type_arg: 0 },
    Target { rva: 0xA1C860, prologue: &[0x40, 0x56, 0x48, 0x83, 0xEC, 0x30], name: "netai recv 5", type_arg: 0 },
];

const N: usize = 13;
static CALLS: [AtomicU32; N] = [const { AtomicU32::new(0) }; N];
/// Packet type counts for the send/recv hooks: [hook][type 0..127].
static TYPES: [[AtomicU32; 128]; 2] = [const { [const { AtomicU32::new(0) }; 128] }; 2];
/// Callers: (id << 32 | caller rva) and counts, open addressing.
const CALLER_SLOTS: usize = 256;
static CALLER_KEY: [AtomicUsize; CALLER_SLOTS] = [const { AtomicUsize::new(0) }; CALLER_SLOTS];
static CALLER_N: [AtomicU32; CALLER_SLOTS] = [const { AtomicU32::new(0) }; CALLER_SLOTS];

/// Called from every stub: must never panic, only atomics.
extern "C" fn observed(id: u32, ret: usize, rdx: usize, r8: usize) {
    let i = id as usize;
    if i >= N {
        return;
    }
    CALLS[i].fetch_add(1, Ordering::Relaxed);
    let ty = match TARGETS[i].type_arg {
        2 => Some(rdx as u32),
        3 => Some(r8 as u32),
        _ => None,
    };
    if let Some(t) = ty
        && i < 2
    {
        TYPES[i][(t as usize).min(127)].fetch_add(1, Ordering::Relaxed);
    }
    let base = module_base();
    let rva = ret.wrapping_sub(base);
    if rva < 0x4000000 {
        let key = (i + 1) << 32 | rva;
        let mut h = (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as usize % CALLER_SLOTS;
        for _ in 0..16 {
            match CALLER_KEY[h].compare_exchange(0, key, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => {
                    CALLER_N[h].fetch_add(1, Ordering::Relaxed);
                    return;
                }
                Err(k) if k == key => {
                    CALLER_N[h].fetch_add(1, Ordering::Relaxed);
                    return;
                }
                Err(_) => h = (h + 1) % CALLER_SLOTS,
            }
        }
    }
}

/// Entry hook: `jmp` to a stub that saves the volatile registers, calls
/// `observed(id, return address, rdx, r8)`, restores them, runs the relocated
/// prologue and jumps back after it.
fn install_one(id: u32, t: &Target) -> bool {
    let base = module_base();
    let site = base + t.rva;
    let found: Vec<u8> = (0..t.prologue.len()).filter_map(|k| read::<u8>(site + k)).collect();
    if found != t.prologue {
        log::line(&format!("observe: {} at {site:#x} has unexpected bytes {found:02x?}, not hooked", t.name));
        return false;
    }
    let Some(stub) = crate::hook::alloc_near_pub(site) else { return false };
    let mut c: Vec<u8> = Vec::with_capacity(96);
    c.extend_from_slice(&[0x51, 0x52, 0x41, 0x50, 0x41, 0x51, 0x50, 0x41, 0x52, 0x41, 0x53]); // push rcx rdx r8 r9 rax r10 r11
    c.extend_from_slice(&[0x48, 0x83, 0xEC, 0x20]); // sub rsp, 20h
    c.push(0xB9);
    c.extend_from_slice(&id.to_le_bytes()); // mov ecx, id
    c.extend_from_slice(&[0x48, 0x8B, 0x54, 0x24, 0x58]); // mov rdx, [rsp+58h] (return address)
    c.extend_from_slice(&[0x4C, 0x8B, 0x44, 0x24, 0x48]); // mov r8, [rsp+48h] (caller rdx)
    c.extend_from_slice(&[0x4C, 0x8B, 0x4C, 0x24, 0x40]); // mov r9, [rsp+40h] (caller r8)
    c.extend_from_slice(&[0x48, 0xB8]);
    c.extend_from_slice(&(observed as *const () as usize as u64).to_le_bytes()); // mov rax, observed
    c.extend_from_slice(&[0xFF, 0xD0]); // call rax
    c.extend_from_slice(&[0x48, 0x83, 0xC4, 0x20]); // add rsp, 20h
    c.extend_from_slice(&[0x41, 0x5B, 0x41, 0x5A, 0x58, 0x41, 0x59, 0x41, 0x58, 0x5A, 0x59]); // pop r11 r10 rax r9 r8 rdx rcx
    c.extend_from_slice(t.prologue); // the original instructions
    c.extend_from_slice(&[0xFF, 0x25, 0, 0, 0, 0]); // jmp [rip+0]
    c.extend_from_slice(&((site + t.prologue.len()) as u64).to_le_bytes());
    unsafe { std::ptr::copy_nonoverlapping(c.as_ptr(), stub as *mut u8, c.len()) };
    // 5-byte jmp at the site, the rest of the prologue padded with int3 (never
    // reached: the stub jumps back past it).
    crate::hook::patch_jmp_pub(site, stub, t.prologue.len())
}

pub fn install() {
    // native_session = 1 at start: nsession.rs hooks the broadcast / member recv
    // entries itself (and counts them); two jumps on one prologue cannot coexist.
    let skip = |i: usize| crate::tuning::native_session() && i < 2;
    let ok = TARGETS.iter().enumerate().filter(|(i, t)| !skip(*i) && install_one(*i as u32, t)).count();
    log::line(&format!("observe: read-only netcode observation, {ok}/{} hooks installed", TARGETS.len()));
}

/// Every 10 s from the frame task: what was called since the last log, the
/// packet types, the busiest callers, and the session state as memory.
pub fn log_stats() {
    let base = module_base();
    let s = read_ptr(base + 0x3D948E8);
    let fsm = read_ptr(base + 0x3D67C90);
    let game_man = read_ptr(base + 0x3D5C0D0);
    let members = s.and_then(|s| Some((read::<usize>(s + 0x168)?.wrapping_sub(read::<usize>(s + 0x160)?)) / 56));
    log::line(&format!(
        "observe: session S+0x184 {:?} S+0x18 {:?} members {:?} FSM+0xDC {:?} GameMan+0xBA6 (offline) {:?}",
        s.and_then(|s| read::<i32>(s + 0x184)),
        s.and_then(|s| read::<[u8; 8]>(s + 0x18)),
        members,
        fsm.and_then(|f| read::<i32>(f + 0xDC)),
        game_man.and_then(|g| read::<u8>(g + 0xBA6)),
    ));
    let mut parts = Vec::new();
    for (i, t) in TARGETS.iter().enumerate() {
        let n = CALLS[i].swap(0, Ordering::Relaxed);
        if n > 0 {
            parts.push(format!("{} {n}", t.name));
        }
    }
    log::line(&format!("observe: calls (10 s): {}", if parts.is_empty() { "none".into() } else { parts.join(", ") }));
    for (k, label) in ["send types", "recv types"].iter().enumerate() {
        let types: Vec<String> = (0..128)
            .filter_map(|t| {
                let n = TYPES[k][t].swap(0, Ordering::Relaxed);
                (n > 0).then(|| format!("{t}:{n}"))
            })
            .collect();
        if !types.is_empty() {
            log::line(&format!("observe: {label} {}", types.join(" ")));
        }
    }
    let mut callers: Vec<(u32, usize, usize)> = (0..CALLER_SLOTS)
        .filter_map(|h| {
            let key = CALLER_KEY[h].load(Ordering::Acquire);
            let n = CALLER_N[h].swap(0, Ordering::Relaxed);
            (key != 0 && n > 0).then(|| (n, (key >> 32) - 1, key & 0xFFFF_FFFF))
        })
        .collect();
    callers.sort_by(|a, b| b.0.cmp(&a.0));
    if !callers.is_empty() {
        let top: Vec<String> = callers.iter().take(16).map(|(n, i, rva)| format!("{}<-{rva:#x}:{n}", TARGETS[*i].name)).collect();
        log::line(&format!("observe: callers {}", top.join(", ")));
    }
}
