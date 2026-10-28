//! State behind the 'Mod settings' panel (v8.31a): shared between the game task (puppet.rs
//! publishes the values, applies the edits) and the render thread (ui.rs draws the panel).
//! Everything here is plain data behind a mutex; the render thread never touches game
//! memory and never the ini file.

use std::sync::Mutex;

/// What this machine is in the session. Only a host (or a player with no session) edits.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Role {
    #[default]
    Solo,
    Host,
    Client,
}

/// The settings the panel edits (ini keys `enemy_health_scaling`, `enemy_posture_scaling`,
/// `boss_health_scaling`, `boss_posture_scaling`, `death_debuffs`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Values {
    pub enemy_hp: u16,
    pub enemy_posture: u16,
    pub boss_hp: u16,
    pub boss_posture: u16,
    pub death_debuffs: bool,
}

impl Default for Values {
    fn default() -> Self {
        Self { enemy_hp: 60, enemy_posture: 60, boss_hp: 50, boss_posture: 50, death_debuffs: true }
    }
}

#[derive(Default)]
struct Shared {
    role: Role,
    mine: Values,
    host: Option<Values>,
    system_menu_open: bool,
}

static SHARED: Mutex<Option<Shared>> = Mutex::new(None);
static EDIT: Mutex<Option<Values>> = Mutex::new(None);
static SAVE_RESULT: Mutex<String> = Mutex::new(String::new());

/// Game task, every frame: the role, this machine's values, the host's values (a client
/// only; `None` until they arrived) and whether the game's System list is open.
pub fn publish(role: Role, mine: Values, host: Option<Values>, system_menu_open: bool) {
    if let Ok(mut s) = SHARED.lock() {
        *s = Some(Shared { role, mine, host, system_menu_open });
    }
}

/// Render thread: `(role, mine, host, system_menu_open)`.
pub fn snapshot() -> (Role, Values, Option<Values>, bool) {
    SHARED
        .lock()
        .ok()
        .and_then(|s| s.as_ref().map(|s| (s.role, s.mine, s.host, s.system_menu_open)))
        .unwrap_or((Role::Solo, Values::default(), None, false))
}

/// Render thread: the Apply button. The game task picks it up next frame.
pub fn post_edit(v: Values) {
    if let Ok(mut e) = EDIT.lock() {
        *e = Some(v);
    }
}

/// Game task: the pending edit, once.
pub fn take_edit() -> Option<Values> {
    EDIT.lock().ok().and_then(|mut e| e.take())
}

/// Only the host (or a player with no session) may change the settings.
pub fn can_edit(r: Role) -> bool {
    matches!(r, Role::Solo | Role::Host)
}

/// A typed or dragged percentage into the ini's range 0..=1000.
pub fn clamp_pct(v: i64) -> u16 {
    v.clamp(0, 1000) as u16
}

pub fn set_save_result(s: String) {
    if let Ok(mut r) = SAVE_RESULT.lock() {
        *r = s;
    }
}

/// The last save result for the panel's text line ("" = nothing yet).
pub fn save_result() -> String {
    SAVE_RESULT.lock().map(|r| r.clone()).unwrap_or_default()
}

/// The ini keys of an edit, as `settings::write_keys` takes them.
pub fn ini_updates(v: Values) -> Vec<(&'static str, String)> {
    vec![
        ("enemy_health_scaling", v.enemy_hp.to_string()),
        ("enemy_posture_scaling", v.enemy_posture.to_string()),
        ("boss_health_scaling", v.boss_hp.to_string()),
        ("boss_posture_scaling", v.boss_posture.to_string()),
        ("death_debuffs", (v.death_debuffs as u8).to_string()),
    ]
}

/// A stamp younger than this counts as 'the System list is being drawn now'.
pub const STAMP_FRESH_MS: u64 = 500;
/// The latch never stays on longer than this (a stuck state must not keep the mouse).
pub const LATCH_MAX_MS: u64 = 10 * 60 * 1000;

/// One frame's inputs of the 'System list open' latch.
#[derive(Clone, Copy, Debug)]
pub struct LatchIn {
    /// Age of the last Quit-command predicate call (quitblock.rs); `None` = never.
    pub stamp_age_ms: Option<u64>,
    /// `game::menu_mask()`.
    pub mask: Option<u32>,
    pub load_pending: bool,
    pub in_world: bool,
    pub dead: bool,
    /// How long the latch has been on (0 while off).
    pub held_ms: u64,
}

/// The latch: set by a fresh stamp, cleared when the menus are all closed (mask 0), a
/// load is pending, we are not in the world or dead, or after `LATCH_MAX_MS`. Returns the
/// new state and, on a transition, the reason for the log. The stamp alone never closes
/// it (how often the game asks the predicate while the list is idle is not known).
pub fn latch_step(open: bool, i: LatchIn) -> (bool, Option<&'static str>) {
    let blocker = if !i.in_world {
        Some("not in the world")
    } else if i.load_pending {
        Some("load pending")
    } else if i.dead {
        Some("player dead")
    } else if i.mask == Some(0) {
        Some("menu mask 0")
    } else {
        None
    };
    if open {
        if let Some(why) = blocker {
            return (false, Some(why));
        }
        if i.held_ms >= LATCH_MAX_MS {
            return (false, Some("10 min limit"));
        }
        return (true, None);
    }
    let fresh = i.stamp_age_ms.is_some_and(|a| a < STAMP_FRESH_MS);
    if fresh && blocker.is_none() && i.mask.is_some() {
        return (true, Some("system list open"));
    }
    (false, None)
}

// ---- Controller navigation (v8.32a). Only the right stick, the triggers, R3, LB and RB are
// used: the game's own System menu reads the D-pad, A and B at the same moment and the
// pad input cannot be blocked without hooking XInput. ----

use crate::spectate_input::PadState;

/// Right stick beyond this (raw, about 0.55 of full scale) counts as pushed.
pub const NAV_STICK: i32 = 18000;
/// Repeat period of the row movement.
pub const NAV_MOVE_REPEAT_MS: u64 = 300;
/// Hold time before the value starts repeating, and the repeat period after that.
pub const NAV_ADJUST_DELAY_MS: u64 = 350;
pub const NAV_ADJUST_REPEAT_MS: u64 = 90;
/// Trigger: fires at or above `NAV_TRIGGER_FIRE`, re-arms below `NAV_TRIGGER_REARM`.
pub const NAV_TRIGGER_FIRE: u8 = 200;
pub const NAV_TRIGGER_REARM: u8 = 80;
const PAD_LB: u16 = 0x0100;
const PAD_RB: u16 = 0x0200;
const PAD_R3: u16 = 0x0080;

/// Rows of the panel: enemy HP, enemy posture, boss HP, boss posture, death debuff, Apply.
pub const NAV_ROWS: usize = 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavAction {
    /// -1 = up, +1 = down.
    Move(i32),
    /// Percent change of the selected slider.
    Adjust(i32),
    Apply,
    Toggle,
}

/// Pure pad navigator: feed it one `PadState` per frame.
#[derive(Default, Debug)]
pub struct PadNav {
    y_dir: i32,
    y_next_ms: u64,
    x_dir: i32,
    x_next_ms: u64,
    rt_ready: bool,
    r3_down: bool,
}

impl PadNav {
    pub fn step(&mut self, s: Option<PadState>, now_ms: u64) -> Vec<NavAction> {
        let Some(p) = s else {
            *self = PadNav::default();
            return Vec::new();
        };
        let mut out = Vec::new();
        // Right stick Y: positive = up.
        let ydir = if (p.ry as i32) > NAV_STICK { -1 } else if (p.ry as i32) < -NAV_STICK { 1 } else { 0 };
        if ydir == 0 {
            self.y_dir = 0;
        } else if ydir != self.y_dir || now_ms >= self.y_next_ms {
            self.y_dir = ydir;
            self.y_next_ms = now_ms + NAV_MOVE_REPEAT_MS;
            out.push(NavAction::Move(ydir));
        }
        // Right stick X: value.
        let xdir = if (p.rx as i32) > NAV_STICK { 1 } else if (p.rx as i32) < -NAV_STICK { -1 } else { 0 };
        if xdir == 0 {
            self.x_dir = 0;
        } else {
            let first = xdir != self.x_dir;
            if first || now_ms >= self.x_next_ms {
                let step = if p.buttons & (PAD_LB | PAD_RB) != 0 { 25 } else { 5 };
                self.x_next_ms = now_ms + if first { NAV_ADJUST_DELAY_MS } else { NAV_ADJUST_REPEAT_MS };
                self.x_dir = xdir;
                out.push(NavAction::Adjust(xdir * step));
            }
        }
        // Right trigger: edge with hysteresis. Starts disarmed, so a trigger already held
        // when the panel opens does nothing.
        if p.rt < NAV_TRIGGER_REARM {
            self.rt_ready = true;
        } else if p.rt >= NAV_TRIGGER_FIRE && self.rt_ready {
            self.rt_ready = false;
            out.push(NavAction::Apply);
        }
        // R3: rising edge.
        let r3 = p.buttons & PAD_R3 != 0;
        if r3 && !self.r3_down {
            out.push(NavAction::Toggle);
        }
        self.r3_down = r3;
        out
    }
}

/// Applies one navigator action to the selected row and the draft.
/// Returns `(new selected row, new draft, apply requested)`.
pub fn apply_nav(selected: usize, d: Values, a: NavAction) -> (usize, Values, bool) {
    let mut d = d;
    let mut sel = selected.min(NAV_ROWS - 1);
    match a {
        NavAction::Move(m) => sel = (sel as i32 + m).rem_euclid(NAV_ROWS as i32) as usize,
        NavAction::Adjust(n) => {
            let f = |v: u16| clamp_pct(v as i64 + n as i64);
            match sel {
                0 => d.enemy_hp = f(d.enemy_hp),
                1 => d.enemy_posture = f(d.enemy_posture),
                2 => d.boss_hp = f(d.boss_hp),
                3 => d.boss_posture = f(d.boss_posture),
                _ => {}
            }
        }
        NavAction::Toggle => {
            if sel == 4 {
                d.death_debuffs = !d.death_debuffs;
            }
        }
        NavAction::Apply => return (sel, d, true),
    }
    (sel, d, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(buttons: u16, rt: u8, rx: i16, ry: i16) -> Option<PadState> {
        Some(PadState { buttons, lt: 0, rt, rx, ry })
    }

    #[test]
    fn nav_no_pad_and_centred() {
        let mut n = PadNav::default();
        assert!(n.step(None, 0).is_empty());
        assert!(n.step(pad(0, 0, 0, 0), 10).is_empty());
        assert!(n.step(pad(0, 0, 17999, -17999), 20).is_empty());
    }

    #[test]
    fn nav_move_edge_then_repeat() {
        let mut n = PadNav::default();
        let cases: Vec<(u64, i16, Vec<NavAction>)> = vec![
            (0, 20000, vec![NavAction::Move(-1)]),
            (16, 20000, vec![]),
            (299, 20000, vec![]),
            (300, 20000, vec![NavAction::Move(-1)]),
            (500, 20000, vec![]),
            (600, 20000, vec![NavAction::Move(-1)]),
            (610, 0, vec![]),
            (620, -20000, vec![NavAction::Move(1)]),
            (630, 20000, vec![NavAction::Move(-1)]),
        ];
        for (t, ry, want) in cases {
            assert_eq!(n.step(pad(0, 0, 0, ry), t), want, "t {t}");
        }
    }

    #[test]
    fn nav_adjust_timing_and_boost() {
        let mut n = PadNav::default();
        let a = |v| vec![NavAction::Adjust(v)];
        assert_eq!(n.step(pad(0, 0, 20000, 0), 0), a(5));
        assert_eq!(n.step(pad(0, 0, 20000, 0), 349), vec![]);
        assert_eq!(n.step(pad(0, 0, 20000, 0), 350), a(5));
        assert_eq!(n.step(pad(0, 0, 20000, 0), 439), vec![]);
        assert_eq!(n.step(pad(0, 0, 20000, 0), 440), a(5));
        assert_eq!(n.step(pad(0x0100, 0, 20000, 0), 530), a(25));
        assert_eq!(n.step(pad(0x0200, 0, 20000, 0), 620), a(25));
        assert_eq!(n.step(pad(0, 0, 0, 0), 630), vec![]);
        assert_eq!(n.step(pad(0, 0, -20000, 0), 640), a(-5));
        assert_eq!(n.step(pad(0, 0, -20000, 0), 989), vec![]);
        assert_eq!(n.step(pad(0, 0, -20000, 0), 990), a(-5));
        // Direction flip counts as a first push.
        assert_eq!(n.step(pad(0, 0, 20000, 0), 1000), a(5));
    }

    #[test]
    fn nav_trigger_edge_and_rearm() {
        let mut n = PadNav::default();
        let ap = vec![NavAction::Apply];
        // Held at the start: disarmed.
        assert_eq!(n.step(pad(0, 255, 0, 0), 0), vec![]);
        assert_eq!(n.step(pad(0, 79, 0, 0), 10), vec![]);
        assert_eq!(n.step(pad(0, 199, 0, 0), 20), vec![]);
        assert_eq!(n.step(pad(0, 200, 0, 0), 30), ap);
        assert_eq!(n.step(pad(0, 255, 0, 0), 40), vec![]);
        // Between the thresholds does not re-arm.
        assert_eq!(n.step(pad(0, 100, 0, 0), 50), vec![]);
        assert_eq!(n.step(pad(0, 255, 0, 0), 60), vec![]);
        assert_eq!(n.step(pad(0, 79, 0, 0), 70), vec![]);
        assert_eq!(n.step(pad(0, 255, 0, 0), 80), ap);
    }

    #[test]
    fn nav_r3_edge() {
        let mut n = PadNav::default();
        let tg = vec![NavAction::Toggle];
        assert_eq!(n.step(pad(0x0080, 0, 0, 0), 0), tg);
        assert_eq!(n.step(pad(0x0080, 0, 0, 0), 10), vec![]);
        assert_eq!(n.step(pad(0, 0, 0, 0), 20), vec![]);
        assert_eq!(n.step(pad(0x0080, 0, 0, 0), 30), tg);
        assert_eq!(n.step(None, 40), vec![]);
    }

    #[test]
    fn apply_nav_rows() {
        let d = Values { enemy_hp: 998, enemy_posture: 3, boss_hp: 50, boss_posture: 50, death_debuffs: true };
        assert_eq!(apply_nav(0, d, NavAction::Move(-1)).0, 5);
        assert_eq!(apply_nav(5, d, NavAction::Move(1)).0, 0);
        assert_eq!(apply_nav(0, d, NavAction::Adjust(25)).1.enemy_hp, 1000);
        assert_eq!(apply_nav(1, d, NavAction::Adjust(-5)).1.enemy_posture, 0);
        assert_eq!(apply_nav(2, d, NavAction::Adjust(5)).1.boss_hp, 55);
        assert_eq!(apply_nav(4, d, NavAction::Adjust(5)).1, d);
        assert!(!apply_nav(4, d, NavAction::Toggle).1.death_debuffs);
        assert_eq!(apply_nav(3, d, NavAction::Toggle).1, d);
        assert!(apply_nav(1, d, NavAction::Apply).2);
    }

    #[test]
    fn clamp_pct_range() {
        assert_eq!(clamp_pct(-5), 0);
        assert_eq!(clamp_pct(0), 0);
        assert_eq!(clamp_pct(60), 60);
        assert_eq!(clamp_pct(1000), 1000);
        assert_eq!(clamp_pct(1001), 1000);
        assert_eq!(clamp_pct(i64::MAX), 1000);
    }

    #[test]
    fn only_solo_and_host_edit() {
        assert!(can_edit(Role::Solo));
        assert!(can_edit(Role::Host));
        assert!(!can_edit(Role::Client));
    }

    #[test]
    fn edit_is_taken_once_and_publish_round_trips() {
        let v = Values { enemy_hp: 1, enemy_posture: 2, boss_hp: 3, boss_posture: 4, death_debuffs: false };
        assert_eq!(take_edit(), None);
        post_edit(v);
        assert_eq!(take_edit(), Some(v));
        assert_eq!(take_edit(), None);
        publish(Role::Client, v, Some(Values::default()), true);
        assert_eq!(snapshot(), (Role::Client, v, Some(Values::default()), true));
    }

    #[test]
    fn ini_updates_use_the_scaling_keys() {
        let v = Values { enemy_hp: 10, enemy_posture: 20, boss_hp: 30, boss_posture: 40, death_debuffs: true };
        let u = ini_updates(v);
        assert_eq!(u[0], ("enemy_health_scaling", "10".to_string()));
        assert_eq!(u[3], ("boss_posture_scaling", "40".to_string()));
        assert_eq!(u[4], ("death_debuffs", "1".to_string()));
    }

    fn inp() -> LatchIn {
        LatchIn { stamp_age_ms: Some(100), mask: Some(4), load_pending: false, in_world: true, dead: false, held_ms: 0 }
    }

    #[test]
    fn latch_table() {
        // (open before, input, expected open, expected reason)
        let cases: Vec<(bool, LatchIn, bool, Option<&str>)> = vec![
            (false, inp(), true, Some("system list open")),
            (false, LatchIn { stamp_age_ms: Some(600), ..inp() }, false, None),
            (false, LatchIn { stamp_age_ms: None, ..inp() }, false, None),
            (false, LatchIn { mask: Some(0), ..inp() }, false, None),
            (false, LatchIn { mask: None, ..inp() }, false, None),
            (false, LatchIn { load_pending: true, ..inp() }, false, None),
            (false, LatchIn { in_world: false, ..inp() }, false, None),
            (false, LatchIn { dead: true, ..inp() }, false, None),
            // Open: a stale stamp alone does not close it.
            (true, LatchIn { stamp_age_ms: Some(5000), ..inp() }, true, None),
            (true, LatchIn { stamp_age_ms: None, held_ms: 1000, ..inp() }, true, None),
            (true, LatchIn { mask: Some(0), ..inp() }, false, Some("menu mask 0")),
            (true, LatchIn { load_pending: true, ..inp() }, false, Some("load pending")),
            (true, LatchIn { in_world: false, ..inp() }, false, Some("not in the world")),
            (true, LatchIn { dead: true, ..inp() }, false, Some("player dead")),
            (true, LatchIn { held_ms: LATCH_MAX_MS - 1, ..inp() }, true, None),
            (true, LatchIn { held_ms: LATCH_MAX_MS, ..inp() }, false, Some("10 min limit")),
        ];
        for (n, (open, i, want, why)) in cases.into_iter().enumerate() {
            assert_eq!(latch_step(open, i), (want, why), "case {n}: open {open}, {i:?}");
        }
    }
}
