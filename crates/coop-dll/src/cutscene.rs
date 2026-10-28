//! Cutscene state (docs/IMPLEMENTATION-NOTES.md § Cutscene state, v8.3).
//!
//! Detection [VAR, static IDA]: `SprjRemoImp*` at `base+0x3D8DC08`, `RemoMan = *(SprjRemoImp+0x8)`.
//! Playing = `*(u32*)(RemoMan+0x144) & 1`. Finished = `step == -1 || (step == 1 && pending == 0)`
//! with `step = *(i32*)(RemoMan+0x40)`, `pending = *(u64*)(RemoMan+0xD0)`. A cutscene is active
//! while it plays and is not finished. `active()` only reads memory (no game call); the remo id
//! (`0x140ED1980(SprjRemoImp) -> i32`) is a game call, made once per cutscene, for the log.
//!
//! What the state is used for (all N peers, see `any_in_cutscene`):
//! - the snapshot carries it (bit 1 of the wire byte), so the others hide our puppet while the
//!   cutscene moves the player, and the party list says "(in cutscene)";
//! - warps, expulsions, spectate releases and the boss start gate wait for it (game.rs,
//!   bossrules.rs, bossgate.rs);
//! - after our own cutscene the player is put back at where it began when it fell out of the
//!   world / ended in a block nobody is in (`Tracker`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::game::{self, Chr, module_base, read, read_ptr};
use crate::log;
use crate::peers::PeerSet;

const REMO_IMP_RVA: usize = 0x3D8DC08;
const REMO_MAN: usize = 0x8;
const REMO_STEP: usize = 0x40;
const REMO_PENDING: usize = 0xD0;
const REMO_FLAGS: usize = 0x144;
/// `0x140ED1980(SprjRemoImp) -> i32`: the id of the running remo. Only called while active.
const REMO_ID_RVA: usize = 0xED1980;
const DEAD_LATCH_RVA: usize = 0x3EA0D74;

/// Inactive this long = the cutscene ended (a step flicker is not an end).
const END_DEBOUNCE: Duration = Duration::from_millis(300);
/// After the end: look at where we are once, this long after.
const CHECK_AFTER: Duration = Duration::from_secs(2);
/// After the end: the death latch is watched this long.
const WATCH_FOR: Duration = Duration::from_secs(5);
/// Lower than the start by more than this = fell out of the world.
const FELL_M: f32 = 10.0;
/// A visible peer this close (same block) means we stand where the others are: no re-placing.
const PEER_NEAR_M: f32 = 40.0;
/// The re-placement is written through the UpdateCoords hook for this many frames.
const PIN_FRAMES: u32 = 30;

/// Another player (any peer's latest snapshot) is in a cutscene; set by `tick` every frame.
static PEERS_IN: AtomicBool = AtomicBool::new(false);

/// The three words of the remo manager that decide the state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Raw {
    pub flags: u32,
    pub step: i32,
    pub pending: u64,
}

/// `step == -1 || (step == 1 && pending == 0)`.
pub fn finished(step: i32, pending: u64) -> bool {
    step == -1 || (step == 1 && pending == 0)
}

/// Playing (bit 0 of the flags) and not finished.
pub fn is_active(r: Raw) -> bool {
    r.flags & 1 != 0 && !finished(r.step, r.pending)
}

fn remo_man() -> Option<usize> {
    read_ptr(read_ptr(module_base() + REMO_IMP_RVA)? + REMO_MAN)
}

/// The remo manager's words, `None` while it does not exist (title, loading).
pub fn raw() -> Option<Raw> {
    let m = remo_man()?;
    Some(Raw { flags: read(m + REMO_FLAGS)?, step: read(m + REMO_STEP)?, pending: read(m + REMO_PENDING)? })
}

/// Our own cutscene is running. Safe reads only.
pub fn active() -> bool {
    raw().is_some_and(is_active)
}

/// The id of the running remo (a game call: only while `active()`, frame task, logging only).
pub fn remo_id() -> Option<i32> {
    let imp = read_ptr(module_base() + REMO_IMP_RVA)?;
    let f: extern "C" fn(usize) -> i32 = unsafe { std::mem::transmute(module_base() + REMO_ID_RVA) };
    Some(f(imp))
}

/// Ours or any peer's cutscene (the peers' as of the last `tick`).
pub fn any_in_cutscene() -> bool {
    active() || PEERS_IN.load(Ordering::Relaxed)
}

/// Shifts a start time forward by the frame delta while a cutscene runs, so the timer does
/// not count it (bossrules clocks, the warp wait). `dt` is capped: a hitch is not a cutscene.
pub fn pause(t: &mut Instant, dt: Duration) {
    *t += dt.min(Duration::from_secs(1));
}

// --- Re-placement after our own cutscene ------------------------------------------

/// What `Tracker::step` sees this frame.
pub struct Sample<'a> {
    pub now: Instant,
    pub active: bool,
    /// The remo id, read once at the start edge.
    pub id: Option<i32>,
    pub pos: Option<[f32; 3]>,
    pub angle: f32,
    pub block: Option<i32>,
    /// The game's death latch (`base+0x3EA0D74`) is set.
    pub latch: bool,
    /// (block, position) of every peer with a visible snapshot.
    pub peers: &'a [(i32, [f32; 3])],
}

/// Why the player is put back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    Fell,
    BlockLost,
    Death,
}

impl Why {
    pub fn text(self) -> &'static str {
        match self {
            Why::Fell => "fell more than 10 m below the start",
            Why::BlockLost => "ended in a block no peer is in",
            Why::Death => "death latch within 5 s",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Start {
    pub pos: [f32; 3],
    pub angle: f32,
    pub block: i32,
}

/// What a step reports.
#[derive(Debug, PartialEq, Default)]
pub struct Out {
    /// The cutscene began (its remo id, if read).
    pub started: Option<Option<i32>>,
    /// The cutscene ended after this long.
    pub ended: Option<Duration>,
    /// Put the player back to this.
    pub replace: Option<(Why, Start)>,
}

struct Running {
    since: Instant,
    start: Option<Start>,
    inactive_since: Option<Instant>,
}

struct After {
    ended: Instant,
    start: Start,
    latch0: bool,
    checked: bool,
}

/// The decision at the check: pure. `near_peer` = a visible peer in our block within `PEER_NEAR_M`.
pub fn decide(start: &Start, pos: [f32; 3], block: i32, peer_blocks: &[i32], near_peer: bool, latch_rose: bool) -> Option<Why> {
    if latch_rose {
        return Some(Why::Death);
    }
    if block != start.block && !peer_blocks.contains(&block) {
        return Some(Why::BlockLost);
    }
    if !near_peer && pos[1] < start.pos[1] - FELL_M {
        return Some(Why::Fell);
    }
    None
}

#[derive(Default)]
pub struct Tracker {
    running: Option<Running>,
    after: Option<After>,
}

impl Tracker {
    pub fn running(&self) -> bool {
        self.running.is_some()
    }

    /// Forget everything (a loading screen: the world we saw is gone).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn step(&mut self, s: &Sample) -> Out {
        let mut out = Out::default();
        if s.active {
            // A cutscene right after another one (or one that flickered): the same run.
            if let Some(r) = self.running.as_mut() {
                r.inactive_since = None;
            } else {
                // A new cutscene replaces any pending check of the last one.
                self.after = None;
                let start = s
                    .pos
                    .zip(s.block)
                    .filter(|(p, _)| game::sane_pos(*p))
                    .map(|(pos, block)| Start { pos, angle: s.angle, block });
                self.running = Some(Running { since: s.now, start, inactive_since: None });
                out.started = Some(s.id);
            }
        } else if let Some(r) = self.running.as_mut() {
            let gone = *r.inactive_since.get_or_insert(s.now);
            if s.now.duration_since(gone) >= END_DEBOUNCE {
                let r = self.running.take().expect("running");
                out.ended = Some(gone.duration_since(r.since));
                // Only a cutscene whose start we saw (a position and block) is ever undone.
                self.after = r.start.map(|start| After { ended: gone, start, latch0: s.latch, checked: false });
            }
        }
        if let Some(a) = self.after.as_mut() {
            let elapsed = s.now.saturating_duration_since(a.ended);
            let latch_rose = s.latch && !a.latch0;
            let check_due = !a.checked && elapsed >= CHECK_AFTER;
            let done = elapsed >= WATCH_FOR;
            let why = if !done && (latch_rose || check_due) {
                a.checked |= check_due;
                match (s.pos, s.block) {
                    (Some(pos), Some(block)) => {
                        let peer_blocks: Vec<i32> = s.peers.iter().map(|p| p.0).collect();
                        let near = s.peers.iter().any(|&(b, p)| b == block && game::distance(p, pos) <= PEER_NEAR_M);
                        decide(&a.start, pos, block, &peer_blocks, near, latch_rose)
                    }
                    _ if latch_rose => Some(Why::Death),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(why) = why {
                out.replace = Some((why, a.start));
                self.after = None;
            } else if done {
                self.after = None;
            }
        }
        out
    }
}

/// The frame task's state: the tracker plus the hook pin that holds the re-placement.
#[derive(Default)]
pub struct Cutscene {
    tracker: Tracker,
    pin_frames: u32,
    pin_to: Option<[f32; 3]>,
    logged_peers: bool,
}

/// What the frame task gives `Cutscene::tick`.
pub struct Frame<'a> {
    pub player: Chr,
    pub peers: &'a PeerSet,
    pub here: Option<i32>,
}

impl Cutscene {
    /// A loading screen: nothing of the last world stays, the hook pin lets go.
    pub fn on_loading(&mut self) {
        self.tracker.reset();
        self.release_pin();
        PEERS_IN.store(false, Ordering::Relaxed);
    }

    /// The hook slot of the pin is cleared by a world rebuild (puppet.rs clears every target).
    fn release_pin(&mut self) {
        self.pin_to = None;
        self.pin_frames = 0;
    }

    /// Once per frame in the world (puppet.rs, after the PeerSet is built).
    pub fn tick(&mut self, f: Frame) {
        let now = Instant::now();
        let any_peer = f.peers.iter().any(|p| p.cutscene);
        PEERS_IN.store(any_peer, Ordering::Relaxed);
        if any_peer != self.logged_peers {
            self.logged_peers = any_peer;
            log::line(&format!(
                "cutscene: peers in a cutscene: {:?}",
                f.peers.iter().filter(|p| p.cutscene).map(|p| p.id.slot()).collect::<Vec<_>>()
            ));
        }
        let slot = crate::native::pin_slot(f.peers.me());
        // Hold an earlier re-placement for a few frames (the game's own movement writes over one).
        if self.pin_frames > 0 {
            self.pin_frames -= 1;
            match (self.pin_to, f.player.physics(), f.player.position4()) {
                (Some(p), Some(physics), Some(cur)) if self.pin_frames > 0 => {
                    crate::hook::set_target(slot, physics, [p[0], p[1], p[2], cur[3]]);
                }
                _ => {
                    crate::hook::clear_target(slot);
                    self.pin_to = None;
                }
            }
        }
        let active = active();
        let id = (active && !self.tracker.running()).then(remo_id).flatten();
        let peers: Vec<(i32, [f32; 3])> = f.peers.iter().filter_map(|p| p.snapshot.map(|s| (s.block, s.pos))).collect();
        let out = self.tracker.step(&Sample {
            now,
            active,
            id,
            pos: f.player.position().filter(|&p| game::sane_pos(p)),
            angle: f.player.angle().unwrap_or(0.0),
            block: f.here,
            latch: read::<u32>(module_base() + DEAD_LATCH_RVA).unwrap_or(0) != 0,
            peers: &peers,
        });
        if let Some(id) = out.started {
            log::line(&format!("cutscene: start id {}", id.map_or("?".to_owned(), |i| i.to_string())));
        }
        if let Some(d) = out.ended {
            log::line(&format!("cutscene: end after {:.1} s", d.as_secs_f32()));
        }
        if let Some((why, start)) = out.replace {
            let ok = f.player.set_position(start.pos, start.angle);
            if let Some(physics) = f.player.physics() {
                let w = f.player.position4().map_or(0.0, |c| c[3]);
                crate::hook::set_target(slot, physics, [start.pos[0], start.pos[1], start.pos[2], w]);
                self.pin_to = Some(start.pos);
                self.pin_frames = PIN_FRAMES;
            }
            log::line(&format!(
                "cutscene: re-placed to {:?} in block {} ({}; write ok = {ok})",
                start.pos,
                start.block,
                why.text()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finished_truth_table() {
        assert!(finished(-1, 0));
        assert!(finished(-1, 7));
        assert!(finished(1, 0));
        assert!(!finished(1, 1));
        assert!(!finished(4, 0));
        assert!(!finished(4, 9));
        assert!(!finished(0, 0));
    }

    #[test]
    fn active_needs_playing_and_not_finished() {
        let r = |flags, step, pending| Raw { flags, step, pending };
        assert!(is_active(r(1, 4, 0)));
        assert!(is_active(r(3, 1, 2)));
        assert!(!is_active(r(0, 4, 0)), "not playing");
        assert!(!is_active(r(1, -1, 0)), "finished");
        assert!(!is_active(r(1, 1, 0)), "finished (step 1, nothing pending)");
        assert!(!is_active(r(2, 4, 0)), "only bit 0 counts");
    }

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    fn sample(now: Instant, active: bool, pos: [f32; 3], block: i32, latch: bool, peers: &[(i32, [f32; 3])]) -> Sample<'_> {
        Sample { now, active, id: Some(7), pos: Some(pos), angle: 0.5, block: Some(block), latch, peers }
    }

    #[test]
    fn edges_and_duration_are_reported_once() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        let p = [10.0, 20.0, 30.0];
        assert_eq!(t.step(&sample(at(t0, 0), false, p, 1100, false, &[])), Out::default());
        let o = t.step(&sample(at(t0, 100), true, p, 1100, false, &[]));
        assert_eq!(o.started, Some(Some(7)));
        assert!(t.step(&sample(at(t0, 200), true, p, 1100, false, &[])).started.is_none(), "one start");
        // A flicker shorter than the debounce is not an end.
        assert!(t.step(&sample(at(t0, 5_000), false, p, 1100, false, &[])).ended.is_none());
        assert!(t.step(&sample(at(t0, 5_100), true, p, 1100, false, &[])).ended.is_none());
        t.step(&sample(at(t0, 8_000), false, p, 1100, false, &[]));
        let o = t.step(&sample(at(t0, 8_400), false, p, 1100, false, &[]));
        assert_eq!(o.ended, Some(Duration::from_millis(7_900)));
        assert!(t.step(&sample(at(t0, 8_500), false, p, 1100, false, &[])).ended.is_none());
    }

    fn run_cutscene(t: &mut Tracker, t0: Instant, p0: [f32; 3]) {
        t.step(&sample(at(t0, 0), true, p0, 1100, false, &[]));
        t.step(&sample(at(t0, 4_000), false, p0, 1100, false, &[]));
        let o = t.step(&sample(at(t0, 4_400), false, p0, 1100, false, &[]));
        assert!(o.ended.is_some());
    }

    #[test]
    fn falling_out_of_the_world_is_undone_two_seconds_after() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        let p0 = [100.0, 50.0, 0.0];
        run_cutscene(&mut t, t0, p0);
        let low = [100.0, 30.0, 0.0];
        // Before the 2 s check nothing happens.
        assert!(t.step(&sample(at(t0, 5_000), false, low, 1100, false, &[])).replace.is_none());
        let o = t.step(&sample(at(t0, 6_500), false, low, 1100, false, &[]));
        let (why, start) = o.replace.expect("re-placed");
        assert_eq!(why, Why::Fell);
        assert_eq!(start.pos, p0);
        // Once only.
        assert!(t.step(&sample(at(t0, 7_000), false, low, 1100, false, &[])).replace.is_none());
    }

    #[test]
    fn a_normal_end_and_a_near_peer_are_left_alone() {
        let t0 = Instant::now();
        let p0 = [100.0, 50.0, 0.0];
        let mut t = Tracker::default();
        run_cutscene(&mut t, t0, p0);
        assert!(t.step(&sample(at(t0, 6_500), false, [102.0, 49.0, 1.0], 1100, false, &[])).replace.is_none());
        // 12 m lower but standing next to a peer who watched it too: the cutscene put us there.
        let mut t = Tracker::default();
        run_cutscene(&mut t, t0, p0);
        let low = [100.0, 38.0, 0.0];
        let peers = [(1100, [101.0, 38.0, 2.0])];
        assert!(t.step(&sample(at(t0, 6_500), false, low, 1100, false, &peers)).replace.is_none());
    }

    #[test]
    fn a_block_nobody_is_in_is_undone_but_a_peers_block_is_not() {
        let t0 = Instant::now();
        let p0 = [100.0, 50.0, 0.0];
        let mut t = Tracker::default();
        run_cutscene(&mut t, t0, p0);
        let o = t.step(&sample(at(t0, 6_500), false, p0, 1200, false, &[(1100, p0)]));
        assert_eq!(o.replace.map(|r| r.0), Some(Why::BlockLost));
        let mut t = Tracker::default();
        run_cutscene(&mut t, t0, p0);
        let far = [900.0, 50.0, 0.0];
        assert!(t.step(&sample(at(t0, 6_500), false, p0, 1200, false, &[(1200, far)])).replace.is_none());
    }

    #[test]
    fn the_death_latch_within_five_seconds_counts_but_not_one_already_set() {
        let t0 = Instant::now();
        let p0 = [100.0, 50.0, 0.0];
        let mut t = Tracker::default();
        run_cutscene(&mut t, t0, p0);
        let o = t.step(&sample(at(t0, 5_000), false, p0, 1100, true, &[]));
        assert_eq!(o.replace.map(|r| r.0), Some(Why::Death));
        // The latch was already set when the cutscene ended: not a new event.
        let mut t = Tracker::default();
        t.step(&sample(at(t0, 0), true, p0, 1100, true, &[]));
        t.step(&sample(at(t0, 4_000), false, p0, 1100, true, &[]));
        t.step(&sample(at(t0, 4_400), false, p0, 1100, true, &[]));
        assert!(t.step(&sample(at(t0, 5_000), false, p0, 1100, true, &[])).replace.is_none());
        // And nothing after the watch window.
        let mut t = Tracker::default();
        run_cutscene(&mut t, t0, p0);
        t.step(&sample(at(t0, 6_500), false, p0, 1100, false, &[]));
        assert!(t.step(&sample(at(t0, 10_000), false, p0, 1100, true, &[])).replace.is_none());
    }

    #[test]
    fn a_player_who_never_saw_the_start_is_not_moved() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        // The position was unreadable when it began: no start point, nothing to undo.
        let mut s = sample(at(t0, 0), true, [0.0; 3], 1100, false, &[]);
        s.pos = None;
        t.step(&s);
        t.step(&sample(at(t0, 4_000), false, [0.0, 0.0, 0.0], 1100, false, &[]));
        t.step(&sample(at(t0, 4_400), false, [0.0, 0.0, 0.0], 1100, false, &[]));
        let o = t.step(&sample(at(t0, 6_500), false, [0.0, -100.0, 0.0], 1300, false, &[]));
        assert!(o.replace.is_none());
        // Idle tracker never moves anybody either.
        let mut t = Tracker::default();
        assert!(t.step(&sample(at(t0, 6_500), false, [0.0, -100.0, 0.0], 1300, true, &[])).replace.is_none());
    }

    #[test]
    fn pause_shifts_by_the_frame_delta_but_not_by_a_hitch() {
        let t0 = Instant::now();
        let mut t = t0;
        pause(&mut t, Duration::from_millis(16));
        assert_eq!(t, t0 + Duration::from_millis(16));
        pause(&mut t, Duration::from_secs(30));
        assert_eq!(t, t0 + Duration::from_millis(1016));
    }
}
