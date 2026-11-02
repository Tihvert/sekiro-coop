//! Alive but no control (issue #2, 2026-11-01, Demon of Hatred, NOT verified live): a joiner
//! came in by idol while the host fought; right after the loading screen the screen stayed
//! black, the character did not move and Esc opened nothing, while the overlay still drew the
//! host's HP bar and name. No log of it exists (the game was closed with Alt+F4).
//!
//! The watchdog never lets a living player sit there without a way out:
//! - The signal is the menu key itself: Esc (keyboard) or Start (XInput) pressed while the game
//!   window has the focus, and the menu mask (`0x8C58E0`; the main menu reads 0x800008 in the
//!   issue #2 logs, together with nopause's "a menu pauses the world") not changing within
//!   `PROBE` after it. A menu that opens or closes changes the mask. An unreadable mask is no probe.
//! - `IGNORED_N` ignored presses spread over at least `IGNORED_SPAN` (a throw or a deathblow
//!   animation is shorter) = stuck. Not counted: dead, in the world less than `SETTLE`, our own
//!   cutscene (Esc is the skip there), the 'Mod settings' panel up, a load pending.
//! - Stuck, step 1: one full state line (`stuck:` + statelog's F11 dump) and the mod's own holds
//!   let go (a spectate wait, a held reload / load request, the spectator camera, a stray free
//!   camera switch). Step 2, still stuck `WARP_AFTER` later with more ignored presses: a warp to
//!   our last idol (only with no talk / menu open, the crash of 2026-10-23). Then nothing more
//!   until the next loading screen. A press that does open a menu resets everything.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// After a press, the menu mask has this long to change.
pub const PROBE: Duration = Duration::from_millis(1500);
/// Ignored presses that make "stuck".
pub const IGNORED_N: usize = 3;
/// ... spread over at least this long, all within `IGNORED_WINDOW`.
pub const IGNORED_SPAN: Duration = Duration::from_secs(6);
pub const IGNORED_WINDOW: Duration = Duration::from_secs(40);
/// No counting this soon after the player appeared (the arrival fade, the game's own lock).
pub const SETTLE: Duration = Duration::from_secs(5);
/// Step 2 waits this long after step 1 and needs this many more ignored presses.
pub const WARP_AFTER: Duration = Duration::from_secs(12);
pub const WARP_PRESSES: usize = 2;

/// What the frame task sees this frame.
#[derive(Clone, Copy, Debug)]
pub struct Input {
    pub now: Instant,
    pub alive: bool,
    pub in_world_for: Duration,
    /// Esc / Start went down this frame (game window focused).
    pub press: bool,
    /// The menu mask now (None = unreadable).
    pub mask: Option<u32>,
    /// Presses do not count now: our cutscene, the mod panel, a load pending.
    pub excluded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    /// Control came back after a stuck verdict (log only).
    Back,
    /// Step 1: log the state, let go of the mod's holds.
    Recover,
    /// Step 2: warp to our last idol (if the caller finds it safe).
    Warp,
    /// Step 2 done and still stuck: log once, nothing more until a load.
    GiveUp,
}

#[derive(Default)]
pub struct Stuck {
    /// The press being judged: (when, mask at the press, mask changed since).
    probe: Option<(Instant, Option<u32>, bool)>,
    ignored: VecDeque<Instant>,
    /// 0 = fine, 1 = recovered once, 2 = warped, 3 = gave up.
    step: u8,
    step_at: Option<Instant>,
    /// Ignored presses since the last step.
    since_step: usize,
}

impl Stuck {
    /// A loading screen: a new world, everything starts over.
    pub fn on_loading(&mut self) {
        *self = Self::default();
    }

    #[cfg(test)]
    pub fn step_no(&self) -> u8 {
        self.step
    }

    pub fn ignored_count(&self) -> usize {
        self.ignored.len()
    }

    /// A press is being judged (the caller reads the menu mask only then and on a press).
    pub fn probing(&self) -> bool {
        self.probe.is_some()
    }

    pub fn step(&mut self, i: Input) -> Action {
        if !i.alive || i.in_world_for < SETTLE {
            self.probe = None;
            return Action::None;
        }
        let mut action = Action::None;
        if let Some((at, mask0, changed)) = self.probe.as_mut() {
            if i.mask != *mask0 {
                *changed = true;
            }
            if i.now.duration_since(*at) >= PROBE {
                let (at, changed) = (*at, *changed);
                self.probe = None;
                if changed {
                    // The menu answered: control works.
                    let was = self.step;
                    self.ignored.clear();
                    self.step = 0;
                    self.step_at = None;
                    self.since_step = 0;
                    if was > 0 {
                        action = Action::Back;
                    }
                } else {
                    self.ignored.push_back(at);
                    self.since_step += 1;
                }
            }
        }
        while self.ignored.front().is_some_and(|&t| i.now.duration_since(t) > IGNORED_WINDOW) {
            self.ignored.pop_front();
        }
        if i.press && !i.excluded && i.mask.is_some() && self.probe.is_none() {
            self.probe = Some((i.now, i.mask, false));
        }
        if action != Action::None {
            return action;
        }
        let span = match (self.ignored.front(), self.ignored.back()) {
            (Some(&a), Some(&b)) => b.duration_since(a),
            _ => Duration::ZERO,
        };
        let stuck = self.ignored.len() >= IGNORED_N && span >= IGNORED_SPAN;
        let waited = self.step_at.is_some_and(|t| i.now.duration_since(t) >= WARP_AFTER);
        match self.step {
            0 if stuck => {
                self.step = 1;
                self.step_at = Some(i.now);
                self.since_step = 0;
                Action::Recover
            }
            1 if waited && self.since_step >= WARP_PRESSES => {
                self.step = 2;
                self.step_at = Some(i.now);
                self.since_step = 0;
                Action::Warp
            }
            2 if waited && self.since_step >= WARP_PRESSES => {
                self.step = 3;
                Action::GiveUp
            }
            _ => Action::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Clock {
        t0: Instant,
        ms: u64,
    }

    impl Clock {
        fn new() -> Self {
            Clock { t0: Instant::now(), ms: 0 }
        }
        fn input(&self, press: bool, mask: u32) -> Input {
            Input {
                now: self.t0 + Duration::from_millis(self.ms),
                alive: true,
                in_world_for: Duration::from_secs(60),
                press,
                mask: Some(mask),
                excluded: false,
            }
        }
    }

    /// Presses Esc every `gap_ms` for `n` times with the mask fixed; returns every action seen.
    fn mash(s: &mut Stuck, c: &mut Clock, n: usize, gap_ms: u64, mask: u32) -> Vec<Action> {
        let mut out = Vec::new();
        for _ in 0..n {
            for f in 0..(gap_ms / 50) {
                let a = s.step(c.input(f == 0, mask));
                if a != Action::None {
                    out.push(a);
                }
                c.ms += 50;
            }
        }
        out
    }

    #[test]
    fn ignored_presses_over_six_seconds_are_stuck_then_warp_then_give_up() {
        let (mut s, mut c) = (Stuck::default(), Clock::new());
        // Three ignored presses 3 s apart: the third is judged 1.5 s after it (span 6 s).
        assert_eq!(mash(&mut s, &mut c, 3, 3000, 0), vec![Action::Recover]);
        // Still stuck: two more ignored presses and 12 s later the warp.
        assert_eq!(mash(&mut s, &mut c, 4, 3000, 0), vec![Action::Warp]);
        assert_eq!(mash(&mut s, &mut c, 5, 3000, 0), vec![Action::GiveUp]);
        // Nothing more until a load.
        assert!(mash(&mut s, &mut c, 10, 3000, 0).is_empty());
        s.on_loading();
        assert_eq!(s.step_no(), 0);
    }

    #[test]
    fn a_menu_that_opens_resets_and_reports_control_back() {
        let (mut s, mut c) = (Stuck::default(), Clock::new());
        assert_eq!(mash(&mut s, &mut c, 3, 3000, 0), vec![Action::Recover]);
        // This press opens the main menu (mask 0 -> 0x8).
        let mut seen = Vec::new();
        for f in 0..40 {
            let a = s.step(c.input(f == 0, if f >= 5 { 8 } else { 0 }));
            if a != Action::None {
                seen.push(a);
            }
            c.ms += 50;
        }
        assert_eq!(seen, vec![Action::Back]);
        assert_eq!(s.step_no(), 0);
        assert_eq!(s.ignored_count(), 0);
    }

    #[test]
    fn quick_presses_dead_frames_and_exclusions_do_not_count() {
        // Three ignored presses within 2 s (a throw animation): not stuck.
        let (mut s, mut c) = (Stuck::default(), Clock::new());
        assert!(mash(&mut s, &mut c, 3, 700, 0).is_empty());
        // Dead: nothing.
        let (mut s, mut c) = (Stuck::default(), Clock::new());
        for f in 0..400 {
            let mut i = c.input(f % 60 == 0, 0);
            i.alive = false;
            assert_eq!(s.step(i), Action::None);
            c.ms += 50;
        }
        // Our cutscene / the mod panel: the presses are not probed.
        let (mut s, mut c) = (Stuck::default(), Clock::new());
        for f in 0..400 {
            let mut i = c.input(f % 60 == 0, 0);
            i.excluded = true;
            assert_eq!(s.step(i), Action::None);
            c.ms += 50;
        }
        // Right after the load: nothing.
        let (mut s, mut c) = (Stuck::default(), Clock::new());
        for f in 0..90 {
            let mut i = c.input(f % 30 == 0, 0);
            i.in_world_for = Duration::from_secs(1);
            assert_eq!(s.step(i), Action::None);
            c.ms += 50;
        }
    }

    #[test]
    fn a_closing_menu_is_an_answer_too() {
        // A menu was open (mask 0x8) and Esc closes it: the mask changes, not stuck.
        let (mut s, mut c) = (Stuck::default(), Clock::new());
        for _ in 0..5 {
            for f in 0..60 {
                let mask = if f < 10 { 8 } else { 0 };
                assert_eq!(s.step(c.input(f == 0, mask)), Action::None);
                c.ms += 50;
            }
        }
        assert_eq!(s.ignored_count(), 0);
    }
}
