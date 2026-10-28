//! Resync: both players rest, to clear any desync (2026-10-23, NOT verified live;
//! IMPLEMENTATION-NOTES §22). Elden Ring Seamless Co-op's escape hatch, built on the
//! mirrored rest that already exists (world.rs `apply_rest`, RE-DEATH-RESET.md P2).
//!
//! Trigger: the Kindred Bell used while already connected (items.rs) arms it, a second use
//! within 5 s confirms; with `debug_hotkeys = 1` also F3 (no confirmation). The initiator
//! sends `EV_RESYNC(REQUEST, idol)`; then each side, once the warp gate allows it (no menu,
//! talk, death, load; up to 20 s, `game::WarpWait`), rests:
//! - the idol is loaded here (its map code `idol / 10000` matches our block's idol): the
//!   rest mailbox `REST_REQUEST 0x3AFEB98` + the respawn mailbox `0x3AFEBA0` (world.rs
//!   `resync_rest`): refill, every character and object respawned, the player is put at
//!   the idol. The initiator's idol when it is loaded here, else our last idol;
//! - no idol loaded here: AllChrRespawn `0xA4C0F0` + objects `0x899500` (world.rs
//!   `respawn_all`), no refill, nobody moves.
//!
//! Each side tells the other the result (`DONE`, `SKIPPED`). Not during a boss fight (ours,
//! the friend's, or the death-screen wait): the initiator refuses, the friend skips.
//! Event kind 40: an older build logs it as unknown and simply does not rest (additive).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use crate::game::{self, WarpGate};
use crate::net::Net;
use crate::world::World;
use crate::{bossrules, log, tuning};

/// Game event kind (world.rs dispatches it here). 40: far from world.rs's 1-7 and any
/// small number another feature might take next.
pub const EV_RESYNC: u8 = 40;
const REQUEST: i32 = 1;
const DONE: i32 = 2;
const SKIPPED: i32 = 3;

/// The second Bell use must come this soon after the first...
const CONFIRM_WITHIN: Duration = Duration::from_secs(5);
/// ...but not sooner than this (one use reported twice is not a confirmation).
const CONFIRM_AFTER: Duration = Duration::from_millis(700);
/// No new resync this soon after the last one.
const COOLDOWN: Duration = Duration::from_secs(15);

const VK_F3: i32 = 0x72;

#[link(name = "user32")]
unsafe extern "system" {
    fn GetAsyncKeyState(vk: i32) -> i16;
}

/// Why a rest was skipped (`SKIPPED`, b).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Skip {
    BossFight = 1,
    NoSafeMoment = 2,
}

fn skip_text(b: i32) -> &'static str {
    match b {
        1 => "boss fight",
        2 => "no safe moment (menu / talk / death) within 20 s",
        _ => "unknown",
    }
}

/// `pending`: who asked and the initiator's idol (or -1).
#[derive(Default)]
struct State {
    armed_at: Option<Instant>,
    local_request: Option<&'static str>,
    pending: Option<(&'static str, i32, Instant)>,
    last_done: Option<Instant>,
    wait: game::WarpWait,
    key_down: bool,
    /// Our own request is out; the friend's answer is logged when it comes.
    asked_at: Option<Instant>,
    epoch: u64,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// 1 while a confirmation is open (items.rs reads it lock-free to skip its item cooldown).
static ARMED: AtomicU8 = AtomicU8::new(0);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let mut g = STATE.lock().ok()?;
    Some(f(g.get_or_insert_with(State::default)))
}

/// A Bell use would confirm a resync now (items.rs: skip the 3 s item cooldown).
pub fn confirm_pending() -> bool {
    ARMED.load(Ordering::Acquire) != 0
}

/// The Kindred Bell was used while connected. Returns the notice, or None when the use
/// came too soon after the first one (counted as the same use, ignored).
pub fn bell_used() -> Option<String> {
    with(|st| {
        let now = Instant::now();
        match st.armed_at {
            Some(t) if now.duration_since(t) < CONFIRM_AFTER => None,
            Some(t) if now.duration_since(t) <= CONFIRM_WITHIN => {
                st.armed_at = None;
                ARMED.store(0, Ordering::Release);
                st.local_request = Some("bell");
                log::line("resync: confirmed with the Kindred Bell");
                Some("Resyncing the world: both players rest.".to_string())
            }
            _ => {
                st.armed_at = Some(now);
                ARMED.store(1, Ordering::Release);
                log::line("resync: Kindred Bell used while connected, waiting for a second use to confirm");
                Some("Use again to resync the world (both rest).".to_string())
            }
        }
    })
    .flatten()
}

/// The Kindred Bell's yes/no dialog said yes (items.rs, `confirm_items = 1`): start a
/// resync now, no second use needed. The checks (boss fight, cooldown) run in `tick`.
pub fn request_confirmed() {
    with(|st| {
        st.armed_at = None;
        ARMED.store(0, Ordering::Release);
        st.local_request = Some("bell dialog");
    });
    log::line("resync: confirmed in the Kindred Bell dialog");
}

/// world.rs: the friend's `EV_RESYNC`.
pub fn on_event(a: i32, b: i32) {
    with(|st| match a {
        REQUEST => {
            if st.pending.is_some() {
                log::line(&format!("resync: the friend asked (idol {b}) while ours is still pending; one rest covers both"));
                return;
            }
            log::line(&format!("resync: requested by the friend (their idol {b})"));
            st.pending = Some(("friend", b, Instant::now()));
            st.wait.reset();
            bossrules::notice("Your companion resyncs the world: resting.".into(), 5);
        }
        DONE => {
            log::line(&format!(
                "resync: the friend rested ({}){}",
                if b == 1 { "idol rest" } else { "enemies and objects reset in place" },
                st.asked_at.take().map(|t| format!(", {:.1} s after our request", t.elapsed().as_secs_f32())).unwrap_or_default()
            ));
        }
        SKIPPED => {
            st.asked_at = None;
            log::line(&format!("resync: the friend skipped it ({})", skip_text(b)));
            bossrules::notice(format!("Your companion could not rest ({}).", skip_text(b)), 5);
        }
        _ => log::line(&format!("resync: unknown event ({a}, {b})")),
    });
}

fn in_boss_fight(world: &World) -> bool {
    game::boss_state().is_some() || world.any_peer_boss() || bossrules::spectating() || world.any_peer_down()
}

/// The idol whose rest mailbox works here: the friend's (`theirs`, the same rule as the
/// mirrored rest) or our last idol, when it is in our map (same `idol / 10000` as our block's
/// idol in the join.rs table [I: entity ids `AABxxxx`]). None = no such idol loaded here.
fn idol_here(theirs: i32) -> Option<i32> {
    let here = game::current_block()?;
    let block_idol = crate::join::idol_of_block(here)?;
    let same_map = |idol: i32| idol > 0 && idol / 10_000 == block_idol / 10_000;
    [theirs, game::last_idol()].into_iter().find(|&i| same_map(i))
}

/// Every connected in-world frame (world.rs `on_frame`, game thread). `session`: world.rs'
/// session epoch (a second client joining the host is not a new session).
pub fn tick(world: &mut World, net: &mut Net, session: u64, in_world_for: Duration) {
    let key = tuning::debug_hotkeys() && unsafe { GetAsyncKeyState(VK_F3) } as u16 & 0x8000 != 0;
    let epoch = session;
    let Some((start, rest)) = with(|st| {
        if st.epoch != epoch {
            // A new session: nothing of the old one is pending.
            *st = State { epoch, key_down: st.key_down, last_done: st.last_done, ..State::default() };
            ARMED.store(0, Ordering::Release);
        }
        let pressed = key && !st.key_down;
        st.key_down = key;
        if pressed {
            log::line("resync: F3 (debug_hotkeys)");
            st.local_request = Some("hotkey");
        }
        if st.armed_at.is_some_and(|t| t.elapsed() > CONFIRM_WITHIN) {
            st.armed_at = None;
            ARMED.store(0, Ordering::Release);
            log::line("resync: not confirmed within 5 s, cancelled");
        }
        (st.local_request.take(), st.pending)
    }) else {
        return;
    };

    // Our own request: checks, tell the friend, rest here too.
    if let Some(how) = start {
        let busy = rest.is_some() || with(|st| st.last_done.is_some_and(|t| t.elapsed() < COOLDOWN)).unwrap_or(false);
        if busy {
            log::line(&format!("resync: {how} ignored, a resync ran or is running"));
            bossrules::notice("A resync is already running.".into(), 4);
        } else if in_boss_fight(world) {
            log::line(&format!("resync: {how} refused, boss fight"));
            bossrules::notice("Not during a boss fight.".into(), 5);
        } else {
            let idol = idol_here(-1).unwrap_or(-1);
            net.send_event(EV_RESYNC, REQUEST, idol);
            log::line(&format!(
                "resync: requested by us ({how}), told the friend; our idol {idol}, block {:?}",
                game::current_block()
            ));
            with(|st| {
                st.pending = Some(("us", idol, Instant::now()));
                st.asked_at = Some(Instant::now());
                st.wait.reset();
            });
            // The double-use Bell said so already (`bell_used`).
            if how != "bell" {
                bossrules::notice("Resyncing the world: both players rest.".into(), 5);
            }
        }
    }

    // The rest itself, once the gate allows it.
    let Some((who, theirs, since)) = with(|st| st.pending).flatten() else { return };
    if in_boss_fight(world) {
        log::line(&format!("resync: ({who}) skipped, boss fight"));
        finish(net, Some(Skip::BossFight));
        return;
    }
    match with(|st| st.wait.poll(in_world_for)).unwrap_or(WarpGate::Wait) {
        WarpGate::Wait => {}
        WarpGate::GaveUp => {
            log::line(&format!("resync: ({who}) gave up: no safe moment within 20 s"));
            bossrules::notice("Resync failed: close the menu and try again.".into(), 5);
            finish(net, Some(Skip::NoSafeMoment));
        }
        WarpGate::Go => {
            let (how, ok) = match idol_here(if who == "friend" { theirs } else { -1 }) {
                Some(idol) => (format!("idol rest at {idol}"), world.resync_rest(idol)),
                None => ("enemies and objects reset in place (no idol of ours loaded here)".to_string(), crate::world::respawn_all_noted(-1)),
            };
            log::line(&format!(
                "resync: ({who}) {how} (ok = {ok}), {:.1} s after the request, block {:?}",
                since.elapsed().as_secs_f32(),
                game::current_block()
            ));
            net.send_event(EV_RESYNC, DONE, if how.starts_with("idol") { 1 } else { 2 });
            finish(net, None);
        }
    }
}

fn finish(net: &mut Net, skip: Option<Skip>) {
    if let Some(s) = skip {
        net.send_event(EV_RESYNC, SKIPPED, s as i32);
    }
    with(|st| {
        st.pending = None;
        st.wait.reset();
        st.last_done = Some(Instant::now());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_reasons_have_texts() {
        for s in [Skip::BossFight, Skip::NoSafeMoment] {
            assert_ne!(skip_text(s as i32), "unknown");
        }
        assert_eq!(skip_text(99), "unknown");
    }
}
