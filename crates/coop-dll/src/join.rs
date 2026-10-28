//! Seamless join (client side): the client is kept with the host. On connect,
//! and again whenever the host changes map block for > 3 s or jumps > 30 m at
//! once (idol warp, death respawn, boss wipe) while we are far away, the client
//! is brought over: same block -> teleport; other block -> that block's idol
//! (loading screen) then teleport.
//!
//! Warp: `game::warp_to_idol` (SekiroTool `Functions.Warp`, RVA 0x68E870, with the idol's
//! param index), only through the `game::safe_to_warp` gate (no menu, talk, death or
//! load; at most 20 s of waiting, SPEC-BOSS-RULES §3.3).
//! Idol per block: SekiroTool `IdolIdByAreaIndex` table (area index =
//! FieldArea +0x28, our `game::current_block`).
//! Player teleport inside a block: write the physics position directly, as
//! SekiroTool's `RestorePos` does for the player.

use std::time::{Duration, Instant};

use crate::game::{self, Chr, WarpGate};
use crate::log;

const IDOL_BY_BLOCK: [(i32, i32); 9] = [
    (0, 1001953),
    (1, 1101952),
    (2, 1111950),
    (3, 1121950),
    (6, 1301950),
    (7, 1501952),
    (9, 1701952),
    (11, 2001953),
    (12, 2501953),
];
const WARP_TIMEOUT_SECS: u64 = 90;
const JOIN_OFFSET: f32 = 1.5;
const OTHER_BLOCK_GRACE_SECS: f32 = 3.0;
const HOST_JUMP_M: f32 = 30.0;
const FAR_FROM_HOST_M: f32 = 30.0;
/// After a warp's loading screen, stay at the idol this long before the
/// teleport: the player has just been placed and the area around the target
/// is still streaming in (writing the position at once raced the game's own
/// placement and could land us where collision was not loaded yet).
const TELEPORT_AFTER_LOAD: Duration = Duration::from_secs(3);
/// Time given to the join rest (fade, enemies reset) before the teleport.
const REST_WAIT: Duration = Duration::from_secs(8);
/// No following in either direction this long after a (re)connect: the joiner
/// warps, rests and teleports, which looked like an idol warp to the host.
const JOIN_GRACE: Duration = Duration::from_secs(45);
/// A friend's idol warp is followed only if they are still far from us this
/// long after they appeared. A joiner (or a follower) first appears at the
/// idol and teleports next to us TELEPORT_AFTER_LOAD later; following that
/// "warp" sent us to our own idol and the two players chased each other.
const FOLLOW_CONFIRM: Duration = Duration::from_secs(6);
/// A new connection within this long of the previous one's last frame, after
/// that one finished its join, is the same play session: no join warp or rest.
/// Live 2026-10-24: the host's death reload froze its game thread 13.7 s, the
/// client's link timed out and came back 9 s later; the join logic took it for
/// a first join and warped the client to the host's idol.
const RECONNECT_RESUME: Duration = Duration::from_secs(120);

/// Is a new connection a resume of the previous one (short link drop)?
/// `gap`: time since the previous connection's last frame, `prev_done`: that
/// connection had finished its join (so we were already with the host).
/// `same_session`: no item / title screen changed the session in between (net.rs
/// `session_gen`), so it really was a link drop and not a leave + rejoin.
fn is_reconnect_resume(had_epoch: bool, prev_done: bool, same_session: bool, gap: Option<Duration>) -> bool {
    had_epoch && prev_done && same_session && gap.is_some_and(|g| g < RECONNECT_RESUME)
}

#[derive(Default)]
enum State {
    #[default]
    Idle,
    Warping { since: Instant, target_block: i32 },
    /// Resting at the arrival idol (join rest), then the teleport.
    Resting { since: Instant, target_block: i32 },
    Done,
}

#[derive(Default)]
pub struct Join {
    state: State,
    /// Connection epoch handled; a new connection re-runs the join.
    epoch: u64,
    last_host_pos: Option<[f32; 3]>,
    other_block_since: Option<Instant>,
    /// When the host was last seen at 0 HP: a jump right after that is a death
    /// respawn, which the client must NOT follow (user rule 2026-10-22).
    host_dead_at: Option<Instant>,
    /// Our own last warp/teleport: no following for 15 s after it, so two
    /// players warping at once do not ping-pong.
    last_move: Option<Instant>,
    /// A loading screen was seen since our last warp (see `on_loading`).
    saw_loading: bool,
    warp_from_block: i32,
    /// The friend jumped far away with a loading screen; follow once
    /// FOLLOW_CONFIRM has passed and they are still far.
    follow_pending: Option<Instant>,
    /// Epoch whose "waiting for the host's world state" was logged.
    hold_logged: u64,
    /// Idol to rest at, taken by the caller (world.rs silent_rest).
    rest_request: Option<i32>,
    /// When this connection started: no following during the join (live
    /// 2026-10-23: the host followed the joiner's idol warp and rest).
    connected_at: Option<Instant>,
    /// The join rest belongs to the client's connect-join only, never to a follow.
    rest_on_arrival: bool,
    /// Retry timer while the warp gate says "not now" (menu, talk, ...).
    warp_wait: game::WarpWait,
    /// Last frame `on_frame` ran (connection alive): measures a link drop's length.
    last_frame_at: Option<Instant>,
    /// net.rs `session_gen` at that frame.
    last_frame_gen: u64,
}

impl Join {
    /// Menu: go to the friend now (idol warp of their block + teleport), no join rest.
    pub fn go_to_friend(&mut self) {
        self.rest_on_arrival = false;
        self.follow_pending = None;
        self.state = State::Idle;
        self.warp_wait.reset();
        log::line("join: going to the friend (menu)");
    }

    pub fn take_rest_request(&mut self) -> Option<i32> {
        self.rest_request.take()
    }

    /// Called on loading screens (no player).
    pub fn on_loading(&mut self) {
        self.saw_loading = true;
        // Our own load (death, rest, travel) changes where we are: judge the
        // friend's position afresh afterwards instead of acting on a stale
        // pending follow.
        self.follow_pending = None;
    }

    /// Call every in-world frame on the client with the host's latest snapshot.
    /// Runs on BOTH sides with the other player's snapshot as `host`:
    /// whoever warps to an idol is followed by the other (user rule
    /// 2026-10-22). Only the client is brought over on (re)connect, and a
    /// respawn after death is never followed (each player keeps their idol).
    /// `in_world_for`: time since our player appeared after the last load.
    /// Returns true on the frame we called Warp (a load is about to start:
    /// the caller must let go of every character pointer now).
    /// `hold`: do not start a join/follow warp yet (the host's world state
    /// is still on its way, world.rs `join_hold`; it has its own timeout).
    #[allow(clippy::too_many_arguments)]
    pub fn on_frame(
        &mut self,
        player: Chr,
        epoch: u64,
        host: &coop_net::Snapshot,
        join_on_connect: bool,
        friend_loaded: bool,
        in_world_for: Duration,
        hold: bool,
    ) -> bool {
        if host.hp <= 0 {
            self.host_dead_at = Some(Instant::now());
        }
        let host_recently_died = self.host_dead_at.is_some_and(|t| t.elapsed().as_secs() < 30);
        if epoch != self.epoch {
            let gap = self.last_frame_at.map(|t| t.elapsed());
            let resume = join_on_connect
                && is_reconnect_resume(
                    self.epoch != 0,
                    matches!(self.state, State::Done),
                    self.last_frame_gen == crate::net::session_gen(),
                    gap,
                );
            self.epoch = epoch;
            self.connected_at = Some(Instant::now());
            self.rest_on_arrival = join_on_connect && !resume;
            self.state = if join_on_connect && !resume { State::Idle } else { State::Done };
            if resume {
                log::line(&format!(
                    "join: reconnect after {}s, resuming without a join warp",
                    gap.map_or(0, |g| g.as_secs())
                ));
            }
            self.follow_pending = None;
            self.warp_wait.reset();
        }
        self.last_frame_at = Some(Instant::now());
        self.last_frame_gen = crate::net::session_gen();
        let here = game::current_block();

        // While "with the host", watch for the host leaving us behind.
        let settled = self.last_move.is_none_or(|t| t.elapsed().as_secs() >= 15)
            && self.connected_at.is_none_or(|t| t.elapsed() >= JOIN_GRACE);
        if !matches!(self.state, State::Done) || !settled {
            self.follow_pending = None;
        }
        // Automatic following is off by default (2026-10-23): a mirrored rest moved
        // both players onto their idols, each read the other's jump as a warp, both
        // followed to the block's default idol, and the host crashed mid-menu. Like
        // Elden Ring Seamless, travel is free; the menu has "go to friend".
        if matches!(self.state, State::Done) && settled && crate::tuning::auto_follow() {
            let jumped = self.last_host_pos.is_some_and(|p| game::distance(p, host.pos) > HOST_JUMP_M);
            let far = player.position().is_none_or(|p| game::distance(p, host.pos) > FAR_FROM_HOST_M);
            if here != Some(host.block) {
                self.follow_pending = None;
                let since = *self.other_block_since.get_or_insert_with(Instant::now);
                // Only follow real idol warps: they come with a loading screen. A pit
                // fall-respawn (no loading, no death) must not drag the other player.
                if since.elapsed().as_secs_f32() > OTHER_BLOCK_GRACE_SECS && !host_recently_died && friend_loaded {
                    log::line("join: host is in another area, following");
                    self.state = State::Idle;
                }
            } else {
                self.other_block_since = None;
                if let Some(t) = self.follow_pending {
                    if !far {
                        log::line("join: friend came to us, not following");
                        self.follow_pending = None;
                    } else if host_recently_died {
                        self.follow_pending = None;
                    } else if t.elapsed() >= FOLLOW_CONFIRM {
                        log::line("join: host warped, following");
                        self.follow_pending = None;
                        self.state = State::Idle;
                    }
                } else if jumped && far && host_recently_died {
                    log::line("join: host respawned after a death, staying where we are");
                } else if jumped && far && !friend_loaded {
                    log::line("join: friend jumped without a loading screen (fall respawn?), staying");
                } else if jumped && far {
                    log::line("join: friend warped, following unless they come to us");
                    self.follow_pending = Some(Instant::now());
                }
            }
        }
        self.last_host_pos = Some(host.pos);
        let mut warped = false;
        if !matches!(self.state, State::Idle) {
            self.warp_wait.reset();
        }

        match self.state {
            State::Done => {}
            State::Idle if hold => {
                if self.hold_logged != epoch {
                    self.hold_logged = epoch;
                    log::line("join: waiting for the host's world state before warping");
                }
            }
            State::Idle => {
                // Join and idol-follow go through a loading screen even inside
                // the same area (tuning, default on): the reload rebuilds the
                // world (enemies alive, AI reset), so both start from the same
                // state; a bare teleport kept the joiner's old world (desync).
                let reload = crate::tuning::join_reload();
                if here == Some(host.block) && !reload {
                    teleport_next_to(player, host);
                    self.last_move = Some(Instant::now());
                    self.other_block_since = None;
                    self.state = State::Done;
                } else if let Some(idol) = IDOL_BY_BLOCK.iter().find(|(b, _)| *b == host.block).map(|&(_, table)| {
                    // The host's own last idol when it is in the same map as the table's
                    // idol (same first four digits, e.g. 1101950 temple / 1101952 outskirts).
                    let h = HOST_IDOL.load(std::sync::atomic::Ordering::Relaxed);
                    if h > 0 && h / 1000 == table / 1000 { h } else { table }
                }) {
                    // Live crash 2026-10-23: a warp under an open talk menu freed the
                    // menu's context. The gate waits for a quiet moment (up to 20 s).
                    match self.warp_wait.poll(in_world_for) {
                        WarpGate::Wait => {}
                        WarpGate::GaveUp => self.state = State::Done,
                        WarpGate::Go => {
                            log::line(&format!(
                                "join: host is in block {} (we are in {here:?}), warping to idol {idol}",
                                host.block
                            ));
                            game::warp_to_idol(idol);
                            warped = true;
                            self.last_move = Some(Instant::now());
                            self.state = State::Warping { since: Instant::now(), target_block: host.block };
                            self.saw_loading = false;
                            self.warp_from_block = here.unwrap_or(-1);
                        }
                    }
                } else {
                    log::line(&format!("join: no idol known for host block {}, walk there yourself", host.block));
                    self.state = State::Done;
                }
            }
            State::Warping { since, target_block } => {
                if here == Some(target_block) {
                    // The loading screen is over: put us next to the host.
                    // Same-block warps keep the block index: wait for the load.
                    if (self.saw_loading || target_block != self.warp_from_block) && in_world_for >= TELEPORT_AFTER_LOAD {
                        // The host may have left the block during our load: its
                        // coordinates belong to another map then, and writing
                        // them here dropped us into the void. Stay at the idol;
                        // the other-block rule follows them if it was a warp.
                        if host.block != target_block {
                            log::line(&format!("join: host moved on to block {} during our load, staying at the idol", host.block));
                            self.state = State::Done;
                        } else if let Some(&(_, idol)) =
                            IDOL_BY_BLOCK.iter().find(|(b, _)| *b == target_block).filter(|_| crate::tuning::join_rest() && std::mem::take(&mut self.rest_on_arrival))
                        {
                            // Rest first: our own save's dead enemies come back (the host's
                            // HP mirror kills the ones that are dead there), then follow.
                            log::line(&format!("join: resting at idol {idol} before going to the host"));
                            self.rest_request = Some(idol);
                            self.state = State::Resting { since: Instant::now(), target_block };
                        } else {
                            teleport_next_to(player, host);
                            self.state = State::Done;
                        }
                    }
                } else if since.elapsed().as_secs() > WARP_TIMEOUT_SECS {
                    log::line("join: warp did not arrive in time, giving up");
                    self.state = State::Done;
                }
            }
            State::Resting { since, target_block } => {
                if since.elapsed() >= REST_WAIT && in_world_for >= Duration::from_secs(2) {
                    if here == Some(target_block) && host.block == target_block {
                        teleport_next_to(player, host);
                    } else {
                        log::line("join: after the rest we or the host are in another block, staying at the idol");
                    }
                    self.last_move = Some(Instant::now());
                    self.state = State::Done;
                } else if since.elapsed().as_secs() > WARP_TIMEOUT_SECS {
                    self.state = State::Done;
                }
            }
        }
        warped
    }
}

/// The host's last idol (world.rs `EV_HOST_IDOL`), -1 = unknown.
static HOST_IDOL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

pub(crate) fn set_host_idol(idol: i32) {
    HOST_IDOL.store(idol, std::sync::atomic::Ordering::Relaxed);
}

/// The idol the join warp uses for `block` (resync.rs: which map an idol id belongs to).
pub(crate) fn idol_of_block(block: i32) -> Option<i32> {
    IDOL_BY_BLOCK.iter().find(|(b, _)| *b == block).map(|&(_, idol)| idol)
}

fn teleport_next_to(player: Chr, host: &coop_net::Snapshot) {
    let target = [host.pos[0] + JOIN_OFFSET, host.pos[1], host.pos[2]];
    let ok = player.set_position(target, host.angle);
    log::line(&format!("join: teleported next to the host at {target:?} (ok = {ok})"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_reconnect_resumes() {
        assert!(is_reconnect_resume(true, true, true, Some(Duration::from_secs(25))));
    }

    #[test]
    fn long_gap_or_first_join_or_unfinished_join_warps() {
        assert!(!is_reconnect_resume(true, true, true, Some(Duration::from_secs(121))));
        assert!(!is_reconnect_resume(false, true, true, None));
        assert!(!is_reconnect_resume(true, false, true, Some(Duration::from_secs(5))));
        assert!(!is_reconnect_resume(true, true, true, None));
        // Live 2026-10-24: Severed Thread, then Kindred Charm 36 s later = a new join.
        assert!(!is_reconnect_resume(true, true, false, Some(Duration::from_secs(36))));
    }
}
