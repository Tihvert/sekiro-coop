//! Leaving a companion's world always goes through a loading screen, and the host hears
//! when a companion arrives (2026-10-23, NOT verified live; IMPLEMENTATION-NOTES §26).
//!
//! - Joiner, `[GAMEPLAY] reload_on_leave = 1`: when we stop being a connected client
//!   (Severed Thread, the host closed its world, link lost, title), after the same grace
//!   as the own-world restore (2 s left / 10 s no session / 45 s lost), the world is
//!   reloaded at our last idol, unless a load already came since we left (a title trip,
//!   a death, the own-world merge restore's own reload). The own-world restore goes first
//!   (`ownworld::restore_pending`): a merge reloads by itself, a mirror restore writes
//!   nothing and finishes in the world, and then this reload makes the return visible.
//! - Never mid-boss-fight (our gauge or the death-screen wait): it waits for the fight to
//!   end (or the death / rest load, which counts as the reload). Never during menus,
//!   talk, death, loads: `game::WarpWait` (retried after each 20 s give-up).
//! - Host: the first companion connecting shows a notice (`dialog::game_notice`,
//!   important); the host is not moved while the world stays open. When the host closes its
//!   own world (intent Solo) with a companion connected, it reloads at its last idol through
//!   the same path as the joiner (2026-10-24: Severed Thread gave the host no loading
//!   screen). A companion leaving (intent still Host), a lost link, or nobody connected: no reload.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::dialog::{self, Importance};
use crate::game::{self, WarpGate};
use crate::net::{self, Intent, Link};
use crate::peers::{EpochTracker, PeerSet};
use crate::{bossrules, log};

/// `[GAMEPLAY] reload_on_leave`.
pub static RELOAD_ON_LEAVE: AtomicBool = AtomicBool::new(true);

const GRACE_LEFT: Duration = Duration::from_secs(2);
const GRACE_OFF: Duration = Duration::from_secs(10);
const GRACE_LOST: Duration = Duration::from_secs(45);
const MIN_IN_WORLD: Duration = Duration::from_secs(5);

/// The host closed its world: the link left `HostConnected` while our own intent became
/// Solo. A friend leaving keeps the intent Host, a lost link keeps it too.
fn host_closed(was_host_open: bool, link: Link, intent: Option<Intent>) -> bool {
    was_host_open && !matches!(link, Link::HostConnected | Link::Hosting) && intent == Some(Intent::Solo)
}

/// The player just opened its world (Kindred Bell): the intent changed to Host since the
/// last in-world frame. `prev` None = no frame seen yet this game: an intent already Host
/// then is not a change (v8.17: the reload's own `State` reset made every frame after it
/// look like a new Kindred Bell, an endless reload loop, owner 2026-10-25).
fn world_opened(prev: Option<Option<Intent>>, now: Option<Intent>) -> bool {
    now == Some(Intent::Host) && matches!(prev, Some(p) if p != Some(Intent::Host))
}

#[derive(Default)]
struct State {
    was_client: bool,
    left_at: Option<Instant>,
    loaded_since_left: bool,
    last_in_world: Duration,
    warp: game::WarpWait,
    boss_logged: bool,
    own_logged: bool,
    host_connected: bool,
    /// Our world was open (hosting, with or without a companion) last frame (v8.16: closing
    /// with nobody in it reloads too, owner 2026-10-25).
    host_open: bool,
    /// A new Host intent (Kindred Bell, see `world_opened`) reloads the world too.
    host_opening: bool,
    /// Host: the companions' links seen (a join notice each).
    joins: EpochTracker,
    /// Host closed its own world (Severed Thread) with a companion in it (2026-10-24).
    host_closing: bool,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// The intent seen on the last in-world frame, kept outside `State` so no reset (the
/// reload, reload_on_leave = 0, an earlier load) forgets it. None = no frame seen yet.
static LAST_INTENT: Mutex<Option<Option<Intent>>> = Mutex::new(None);

/// The host's notice for a companion who just joined: the old text while it is the only
/// one (two players, as v7.5), its name once others are here too (PLAN-MULTIPLAYER P4).
fn entered_text(name: &str, peers_now: usize) -> String {
    if peers_now <= 1 || name.is_empty() {
        "A companion has entered your world.".into()
    } else {
        format!("{name} has entered your world.")
    }
}

/// Every in-world frame (puppet.rs, right after the own-world restore). True on the frame
/// we started the reload warp (the caller lets go of every character, like after a join warp).
/// `peers`: this frame's other players (the host notices each join).
pub fn on_frame(in_world_for: Duration, peers: &PeerSet) -> bool {
    let Ok(mut g) = STATE.lock() else { return false };
    let st = g.get_or_insert_with(State::default);
    let link = net::link();

    // Host closing its own world (Severed Thread) with any companion in it: reload like a
    // joiner. A companion leaving while others stay keeps `HostConnected`.
    let now = Instant::now();
    if host_closed(st.host_open, link, net::intent()) && !st.host_closing {
        st.host_closing = true;
        st.left_at = Some(now);
        st.loaded_since_left = false;
        st.warp.reset();
        st.boss_logged = false;
        log::line("leave: host closed its world, companion gone (intent Solo)");
    }
    if link == Link::HostConnected || net::intent() == Some(Intent::Host) {
        st.host_closing = false; // opened again before the reload
    }
    st.host_open = matches!(link, Link::HostConnected | Link::Hosting);
    let intent = net::intent();
    let prev = LAST_INTENT.lock().ok().and_then(|mut l| l.replace(intent));
    if world_opened(prev, intent) && !st.was_client {
        st.host_opening = true;
        st.left_at = Some(now);
        st.loaded_since_left = false;
        st.warp.reset();
        st.boss_logged = false;
        log::line("leave: world opened (Kindred Bell), reloading it through a loading screen");
    }
    // Host: each companion entering our world (its own link).
    if link == Link::HostConnected {
        st.host_connected = true;
        for id in st.joins.update(peers.epochs()).joined {
            let name = peers.by_id(id).map_or("", |p| p.name.as_str());
            log::line(&format!("leave: a companion (peer {}, {name:?}) entered our world (host, no reload)", id.slot()));
            dialog::game_notice(entered_text(name, peers.len()), Importance::Membership, 6);
        }
    } else {
        st.host_connected = false;
        st.joins = EpochTracker::default();
    }

    let loaded = in_world_for < st.last_in_world;
    st.last_in_world = in_world_for;
    if link == Link::ClientConnected {
        if st.left_at.is_some() {
            log::line("leave: connected again, no reload");
        }
        st.was_client = true;
        st.host_closing = false;
        st.host_opening = false;
        st.left_at = None;
        st.loaded_since_left = false;
        st.warp.reset();
        st.boss_logged = false;
        st.own_logged = false;
        return false;
    }
    if !st.was_client && !st.host_closing && !st.host_opening {
        return false;
    }
    let since = *st.left_at.get_or_insert_with(|| {
        log::line(&format!("leave: no longer in the shared world (link {link:?}, intent {:?})", net::intent()));
        now
    });
    st.loaded_since_left |= loaded;
    if !RELOAD_ON_LEAVE.load(Ordering::Relaxed) {
        log::line("leave: reload_on_leave = 0, staying where we are");
        *st = State { last_in_world: in_world_for, ..State::default() };
        return false;
    }
    let grace = match (net::intent(), link) {
        (Some(Intent::Solo | Intent::Host), _) => GRACE_LEFT,
        (_, Link::Off) => GRACE_OFF,
        _ => GRACE_LOST,
    };
    if now.duration_since(since) < grace || in_world_for < MIN_IN_WORLD {
        return false;
    }
    // The own-world restore first (merge: its own reload; mirror: nothing written).
    if st.was_client && crate::ownworld::restore_pending() {
        if !st.own_logged {
            st.own_logged = true;
            log::line("leave: waiting for the own-world restore first");
        }
        return false;
    }
    if st.loaded_since_left {
        log::line("leave: a loading screen already came after leaving (title, death, rest or the own-world reload): no extra reload");
        *st = State { last_in_world: in_world_for, ..State::default() };
        return false;
    }
    if game::boss_state().is_some() || bossrules::spectating() {
        if !st.boss_logged {
            st.boss_logged = true;
            log::line("leave: boss fight, the reload waits until it ends");
        }
        st.warp.reset();
        return false;
    }
    match st.warp.poll(in_world_for) {
        WarpGate::Wait => false,
        WarpGate::GaveUp => {
            log::line("leave: no safe moment for the reload yet, trying again");
            false
        }
        WarpGate::Go => {
            let idol = game::last_idol();
            if st.host_closing {
                log::line(&format!("leave: host closed its world, reloading at idol {idol}"));
            }
            log::line(&format!(
                "leave: reloading at our last idol {idol} ({:.1} s after leaving): back in our own world through a loading screen",
                now.duration_since(since).as_secs_f32()
            ));
            game::warp_to_idol(idol);
            *st = State::default();
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_close_only_when_we_chose_solo() {
        // Severed Thread / close with a companion in: reload.
        assert!(host_closed(true, Link::Off, Some(Intent::Solo)));
        // The friend left, our intent is still Host: no reload.
        assert!(!host_closed(true, Link::Off, Some(Intent::Host)));
        // Closed with nobody connected (Hosting): reload too.
        assert!(host_closed(true, Link::Off, Some(Intent::Solo)));
        // Link merely lost (no intent change): no reload.
        assert!(!host_closed(true, Link::Off, None));
        // Our world was not open: no reload.
        assert!(!host_closed(false, Link::Off, Some(Intent::Solo)));
        // Still open: nothing yet.
        assert!(!host_closed(true, Link::HostConnected, Some(Intent::Solo)));
        assert!(!host_closed(true, Link::Hosting, Some(Intent::Solo)));
    }

    #[test]
    fn world_opened_once_per_intent_change() {
        // Kindred Bell from solo / no item yet / after a join: reload once.
        assert!(world_opened(Some(Some(Intent::Solo)), Some(Intent::Host)));
        assert!(world_opened(Some(None), Some(Intent::Host)));
        assert!(world_opened(Some(Some(Intent::Join)), Some(Intent::Host)));
        // Still Host on the next frames (also after the reload's State reset): no reload.
        assert!(!world_opened(Some(Some(Intent::Host)), Some(Intent::Host)));
        // First in-world frame of the game with the intent already Host: no reload.
        assert!(!world_opened(None, Some(Intent::Host)));
        // Any other intent: no reload.
        assert!(!world_opened(Some(Some(Intent::Solo)), Some(Intent::Join)));
        assert!(!world_opened(Some(Some(Intent::Host)), Some(Intent::Solo)));
        assert!(!world_opened(Some(None), None));
    }
}
