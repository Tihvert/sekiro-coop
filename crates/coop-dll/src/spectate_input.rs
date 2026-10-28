//! Input side of the spectator camera (spectate_cam.rs, IMPLEMENTATION-NOTES §41): the
//! game's own camera turn (source A), the XInput right stick (source B), the lock-on
//! toggle (key + R3) and the pure helpers around them. Everything that touches Win32 is in
//! `Poller`; the rest is plain math with unit tests.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// XInput right-stick dead zone (raw units, the usual XINPUT_GAMEPAD_RIGHT_THUMB_DEADZONE).
pub const DEADZONE: i32 = 8000;
/// Stick orbit speed at full deflection and `spectate_orbit_speed = 1.0`.
pub const STICK_DEG_PER_S: f32 = 150.0;
/// A frame whose camera turn is larger than this is a cut / reset, not input.
pub const MAX_JUMP: f32 = std::f32::consts::FRAC_PI_4;
/// Source A saw nothing for this long: the game camera is frozen while dead.
pub const FROZEN_AFTER: Duration = Duration::from_secs(5);
/// Lock toggle debounce.
pub const DEBOUNCE: Duration = Duration::from_millis(250);
/// Target cycling by a camera flick: this much screen yaw within `FLICK_WINDOW_S`.
pub const FLICK_RAD: f32 = 20.0 * std::f32::consts::PI / 180.0;
pub const FLICK_WINDOW_S: f32 = 0.3;
/// After a cycle the flick detector sleeps this long.
const FLICK_COOLDOWN_S: f32 = 0.5;
/// Cycling by stick: x past this on its edge.
pub const STICK_CYCLE: f32 = 0.7;
/// XInput R3 (right thumb click).
const PAD_R3: u16 = 0x0080;
/// XInput D-pad left / right: watch the previous / next living player (multiplayer).
const PAD_LEFT: u16 = 0x0004;
const PAD_RIGHT: u16 = 0x0008;
/// Retry period of XInputGetState while no controller answers (it is slow on older DLLs).
const PAD_RETRY: Duration = Duration::from_secs(1);

/// Mouse look degrees per pixel at `spectate_mouse_sens = 1.0`.
pub const MOUSE_DEG_PER_PX: f32 = 0.15;
/// A cursor move larger than this between two samples is a warp (the game recentering it,
/// a window switch), not input.
pub const MOUSE_MAX_JUMP_PX: i32 = 300;
/// Samples further apart than this carry no usable rate (first sample after a pause).
const MOUSE_MAX_DT_S: f32 = 0.25;
/// Largest stick-equivalent deflection a mouse move may produce (a hard flick is clipped).
pub const MOUSE_AXIS_MAX: f32 = 6.0;
/// Windows virtual keys of the arrow-key fallback.
const VK_LEFT: i32 = 0x25;
const VK_UP: i32 = 0x26;
const VK_RIGHT: i32 = 0x27;
const VK_DOWN: i32 = 0x28;

/// `spectate_mouse`: mouse look and arrow keys steer the spectator camera.
pub static SPECTATE_MOUSE: AtomicBool = AtomicBool::new(true);
/// `spectate_mouse_sens` in thousandths (1000 = `MOUSE_DEG_PER_PX`).
pub static SPECTATE_MOUSE_SENS_MILLI: AtomicU32 = AtomicU32::new(1000);

pub fn wrap_pi(a: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    (a + PI).rem_euclid(TAU) - PI
}

/// Camera forward -> (yaw = atan2(z, x), pitch = asin(y)); None for a non-finite or
/// near-zero vector.
pub fn fwd_angles(f: [f32; 3]) -> Option<(f32, f32)> {
    if !f.iter().all(|v| v.is_finite()) {
        return None;
    }
    let len = (f[0] * f[0] + f[1] * f[1] + f[2] * f[2]).sqrt();
    (len > 0.5).then(|| (f[2].atan2(f[0]), (f[1] / len).clamp(-1.0, 1.0).asin()))
}

/// Turn between two frames of the game camera: (d yaw wrapped, d pitch). None when either
/// is larger than `MAX_JUMP` (the frame is ignored, the caller still stores `cur`).
pub fn angle_delta(last: (f32, f32), cur: (f32, f32)) -> Option<(f32, f32)> {
    let (dy, dp) = (wrap_pi(cur.0 - last.0), cur.1 - last.1);
    (dy.abs() <= MAX_JUMP && dp.abs() <= MAX_JUMP).then_some((dy, dp))
}

/// A turn that counts as input (float noise does not).
pub fn is_motion(dy: f32, dp: f32) -> bool {
    dy.abs() + dp.abs() > 1e-5
}

/// Source A never moved for `FROZEN_AFTER`: switch to the stick.
pub fn frozen_after(engaged_for: Duration, moved: bool) -> bool {
    !moved && engaged_for >= FROZEN_AFTER
}

/// Raw thumb value -> -1..1 with the dead zone removed (and the rest rescaled).
pub fn stick_axis(raw: i16) -> f32 {
    let v = (raw as i32).abs();
    if v <= DEADZONE {
        return 0.0;
    }
    let m = ((v - DEADZONE) as f32 / (32767 - DEADZONE) as f32).min(1.0);
    if raw < 0 { -m } else { m }
}

/// Orbit change from the stick: (d yaw of the view, d pitch of the orbit). Stick right
/// turns the view to its right (yaw falls for a right-handed camera, `hand` = 1); stick up
/// looks up, i.e. the camera swings down (orbit pitch falls).
pub fn stick_orbit(sx: f32, sy: f32, dt: f32, speed: f32, hand: f32) -> (f32, f32) {
    let r = STICK_DEG_PER_S.to_radians() * speed * dt;
    (-hand * sx * r, -sy * r)
}

/// Cursor move since the last sample -> stick-equivalent (sx, sy) so `stick_orbit` turns the
/// view by `dx * MOUSE_DEG_PER_PX * sens` degrees. Right = +x, up (screen y falls) = +y.
/// None for a warp (`MOUSE_MAX_JUMP_PX`) or an unusable `dt`; the axes are clamped.
pub fn mouse_axes(dx: i32, dy: i32, dt: f32, sens: f32) -> Option<(f32, f32)> {
    if dx.abs() > MOUSE_MAX_JUMP_PX || dy.abs() > MOUSE_MAX_JUMP_PX || !(dt > 0.0 && dt <= MOUSE_MAX_DT_S) {
        return None;
    }
    let k = MOUSE_DEG_PER_PX * sens / (STICK_DEG_PER_S * dt);
    Some(((dx as f32 * k).clamp(-MOUSE_AXIS_MAX, MOUSE_AXIS_MAX), (-dy as f32 * k).clamp(-MOUSE_AXIS_MAX, MOUSE_AXIS_MAX)))
}

/// Arrow keys (left, right, up, down held) -> full stick deflection; opposite keys cancel.
pub fn arrow_axes(left: bool, right: bool, up: bool, down: bool) -> (f32, f32) {
    ((right as i8 - left as i8) as f32, (up as i8 - down as i8) as f32)
}

/// Per axis: the pad when it is deflected, else the other source.
pub fn merge_look(pad: (f32, f32), other: (f32, f32)) -> (f32, f32) {
    let pick = |p: f32, o: f32| if p != 0.0 { p } else { o };
    (pick(pad.0, other.0), pick(pad.1, other.1))
}

/// Mouse + arrow keys as one source: zero when the setting is off, the mouse wins over the keys.
pub fn keyboard_look(enabled: bool, mouse: Option<(f32, f32)>, arrows: (f32, f32)) -> (f32, f32) {
    if !enabled {
        return (0.0, 0.0);
    }
    merge_look(mouse.unwrap_or_default(), arrows)
}

/// Rising edge of a level.
pub fn edge(prev: bool, now: bool) -> bool {
    !prev && now
}

/// Is a press `since_last` after the previous accepted one allowed?
pub fn debounce_ok(since_last: Option<Duration>) -> bool {
    since_last.is_none_or(|d| d >= DEBOUNCE)
}

/// Stick x -> (new state -1/0/1, the direction to cycle when it just crossed `STICK_CYCLE`).
pub fn stick_edge(prev: i8, x: f32) -> (i8, Option<i8>) {
    let s = if x > STICK_CYCLE {
        1
    } else if x < -STICK_CYCLE {
        -1
    } else {
        0
    };
    (s, (s != 0 && s != prev).then_some(s))
}

/// Detects a camera flick (source A while locked): `FLICK_RAD` of screen yaw within
/// `FLICK_WINDOW_S`. Screen yaw is positive to the view's right.
#[derive(Default)]
pub struct Flick {
    acc: f32,
    start: Option<f32>,
    sleep_until: f32,
}

impl Flick {
    /// `t`: seconds on any monotonic clock; `d_screen`: this frame's turn (0 is fine).
    pub fn push(&mut self, t: f32, d_screen: f32) -> Option<i8> {
        if t < self.sleep_until {
            return None;
        }
        if self.start.is_none_or(|s| t - s > FLICK_WINDOW_S) {
            self.start = Some(t);
            self.acc = 0.0;
        }
        self.acc += d_screen;
        if self.acc.abs() > FLICK_RAD {
            let dir = if self.acc > 0.0 { 1 } else { -1 };
            self.acc = 0.0;
            self.start = None;
            self.sleep_until = t + FLICK_COOLDOWN_S;
            return Some(dir);
        }
        None
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

// --- Win32 -----------------------------------------------------------------------------

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn GetCurrentProcessId() -> u32;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetAsyncKeyState(vk: i32) -> i16;
    fn GetForegroundWindow() -> *mut c_void;
    fn GetWindowThreadProcessId(hwnd: *mut c_void, pid: *mut u32) -> u32;
    fn GetCursorPos(pt: *mut [i32; 2]) -> i32;
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct XState {
    packet: u32,
    buttons: u16,
    lt: u8,
    rt: u8,
    lx: i16,
    ly: i16,
    rx: i16,
    ry: i16,
}

type XInputGetState = unsafe extern "system" fn(u32, *mut XState) -> u32;

/// `XInputGetState` of xinput1_4.dll (fallback xinput9_1_0.dll), looked up once.
fn xinput() -> Option<XInputGetState> {
    static F: OnceLock<usize> = OnceLock::new();
    let addr = *F.get_or_init(|| {
        for dll in ["xinput1_4.dll", "xinput9_1_0.dll"] {
            let w: Vec<u16> = dll.encode_utf16().chain(std::iter::once(0)).collect();
            let m = unsafe { LoadLibraryW(w.as_ptr()) };
            if m.is_null() {
                continue;
            }
            let p = unsafe { GetProcAddress(m, c"XInputGetState".as_ptr() as *const u8) } as usize;
            if p != 0 {
                return p;
            }
        }
        0
    });
    (addr != 0).then(|| unsafe { std::mem::transmute::<usize, XInputGetState>(addr) })
}

/// The game window has the keyboard focus (our process owns the foreground window).
fn game_focused() -> bool {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return false;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    pid == unsafe { GetCurrentProcessId() }
}

/// One frame of input.
#[derive(Clone, Copy, Default)]
pub struct Sample {
    /// Right stick, dead zone removed (None: no controller).
    pub stick: Option<(f32, f32)>,
    /// Orbit axes: the stick, else mouse look / arrow keys (per axis, the pad wins).
    pub look: (f32, f32),
    /// Lock key or R3 is down right now.
    pub lock_down: bool,
    /// Watch the next player: the switch key or D-pad right is down; the previous one:
    /// D-pad left.
    pub next_down: bool,
    pub prev_down: bool,
}

#[derive(Default)]
pub struct Poller {
    pad_retry_at: Option<Instant>,
    /// Cursor position and time of the previous mouse sample.
    cursor: Option<((i32, i32), Instant)>,
}

fn key_down(vk: i32) -> bool {
    (unsafe { GetAsyncKeyState(vk) } as u16) & 0x8000 != 0
}

impl Poller {
    /// Mouse look (cursor delta since the last sample) and the arrow keys as stick axes.
    /// The cursor is only read, never moved. Zero while `spectate_mouse` is off.
    fn keyboard_mouse(&mut self, now: Instant) -> (f32, f32) {
        if !SPECTATE_MOUSE.load(Ordering::Relaxed) {
            self.cursor = None;
            return keyboard_look(false, None, (0.0, 0.0));
        }
        let mut pt = [0i32; 2];
        let mouse = if unsafe { GetCursorPos(&mut pt) } != 0 {
            let prev = self.cursor.replace(((pt[0], pt[1]), now));
            prev.and_then(|((px, py), t)| {
                let sens = SPECTATE_MOUSE_SENS_MILLI.load(Ordering::Relaxed) as f32 / 1000.0;
                mouse_axes(pt[0] - px, pt[1] - py, now.saturating_duration_since(t).as_secs_f32(), sens)
            })
        } else {
            self.cursor = None;
            None
        };
        let arrows = arrow_axes(key_down(VK_LEFT), key_down(VK_RIGHT), key_down(VK_UP), key_down(VK_DOWN));
        keyboard_look(true, mouse, arrows)
    }

    /// Reads the keyboard / mouse keys `lock_key` and `switch_key` (0 = none) and controller
    /// 0. Nothing is read while the game window is not focused.
    pub fn sample(&mut self, lock_key: i32, switch_key: i32, now: Instant) -> Sample {
        if !game_focused() {
            self.cursor = None;
            return Sample::default();
        }
        let mut out = Sample::default();
        if lock_key > 0 {
            out.lock_down = key_down(lock_key);
        }
        if switch_key > 0 {
            out.next_down = key_down(switch_key);
        }
        let extra = self.keyboard_mouse(now);
        out.look = extra;
        if self.pad_retry_at.is_some_and(|t| now < t) {
            return out;
        }
        if let Some(f) = xinput() {
            let mut st = XState::default();
            if unsafe { f(0, &mut st) } == 0 {
                self.pad_retry_at = None;
                let pad = (stick_axis(st.rx), stick_axis(st.ry));
                out.stick = Some(pad);
                out.look = merge_look(pad, extra);
                out.lock_down |= st.buttons & PAD_R3 != 0;
                out.next_down |= st.buttons & PAD_RIGHT != 0;
                out.prev_down |= st.buttons & PAD_LEFT != 0;
            } else {
                self.pad_retry_at = Some(now + PAD_RETRY);
            }
        }
        out
    }
}

/// One snapshot of an XInput controller: buttons, triggers (0..255) and the right stick
/// (raw, -32768..32767; positive Y = up).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct PadState {
    pub buttons: u16,
    pub lt: u8,
    pub rt: u8,
    pub rx: i16,
    pub ry: i16,
}

/// State of the first connected XInput controller (user index 0..3), None when none
/// answers. Cheap enough for once per frame: after a miss it does not ask again for
/// `PAD_RETRY`. Does not need the game window to be focused. Never panics.
pub fn pad_state() -> Option<PadState> {
    use std::sync::Mutex;
    static RETRY_AT: Mutex<Option<Instant>> = Mutex::new(None);
    let now = Instant::now();
    let mut retry = RETRY_AT.lock().ok()?;
    if retry.is_some_and(|t| now < t) {
        return None;
    }
    let f = xinput()?;
    for user in 0..4u32 {
        let mut st = XState::default();
        if unsafe { f(user, &mut st) } == 0 {
            *retry = None;
            return Some(PadState { buttons: st.buttons, lt: st.lt, rt: st.rt, rx: st.rx, ry: st.ry });
        }
    }
    *retry = Some(now + PAD_RETRY);
    None
}

/// wButtons of the first connected XInput controller, None when none answers.
pub fn pad_buttons_down() -> Option<u16> {
    pad_state().map(|p| p.buttons)
}

/// Pad button edge for the F11 mark: (new previous level, rising edge). `mask` 0 = off;
/// no controller counts as released.
pub fn pad_edge(prev: bool, buttons: Option<u16>, mask: u16) -> (bool, bool) {
    let down = mask != 0 && buttons.is_some_and(|b| b & mask == mask);
    (down, edge(prev, down))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    #[test]
    fn wrap_stays_in_range() {
        assert!((wrap_pi(3.0 * PI) - PI).abs() < 1e-4 || (wrap_pi(3.0 * PI) + PI).abs() < 1e-4);
        assert!((wrap_pi(0.3) - 0.3).abs() < 1e-6);
        assert!((wrap_pi(-4.0) - (2.0 * PI - 4.0)).abs() < 1e-5);
    }

    #[test]
    fn forward_angles_and_delta() {
        assert_eq!(fwd_angles([0.0; 3]), None);
        assert_eq!(fwd_angles([f32::NAN, 0.0, 1.0]), None);
        let (y, p) = fwd_angles([0.0, 0.0, 1.0]).unwrap();
        assert!((y - PI / 2.0).abs() < 1e-6 && p.abs() < 1e-6);
        let (_, p) = fwd_angles([0.0, 1.0, 0.0]).unwrap();
        assert!((p - PI / 2.0).abs() < 1e-6);
        // Across the +-pi seam the delta is small.
        let d = angle_delta((PI - 0.05, 0.0), (-PI + 0.05, 0.1)).unwrap();
        assert!((d.0 - 0.1).abs() < 1e-5 && (d.1 - 0.1).abs() < 1e-6);
    }

    #[test]
    fn big_jumps_are_rejected() {
        assert!(angle_delta((0.0, 0.0), (0.5, 0.0)).is_some());
        assert!(angle_delta((0.0, 0.0), (0.9, 0.0)).is_none());
        assert!(angle_delta((0.0, 0.0), (0.0, -0.9)).is_none());
        assert!(angle_delta((3.0, 0.0), (-3.0, 0.0)).is_some()); // 0.28 rad over the seam
        assert!(is_motion(0.001, 0.0) && !is_motion(0.0, 0.0));
    }

    #[test]
    fn frozen_needs_five_quiet_seconds() {
        assert!(!frozen_after(Duration::from_secs(4), false));
        assert!(frozen_after(Duration::from_secs(5), false));
        assert!(!frozen_after(Duration::from_secs(60), true));
    }

    #[test]
    fn stick_dead_zone_and_orbit_signs() {
        assert_eq!(stick_axis(8000), 0.0);
        assert_eq!(stick_axis(-5000), 0.0);
        assert!((stick_axis(32767) - 1.0).abs() < 1e-6);
        assert!(stick_axis(-32768) <= -1.0 + 1e-6);
        let half = stick_axis(8000 + (32767 - 8000) / 2);
        assert!((half - 0.5).abs() < 0.01);
        // Full right for one second at speed 1: 150 deg, yaw falls for hand = 1, rises for -1.
        let (dy, dp) = stick_orbit(1.0, 0.0, 1.0, 1.0, 1.0);
        assert!((dy + 150f32.to_radians()).abs() < 1e-5 && dp == 0.0);
        assert!(stick_orbit(1.0, 0.0, 1.0, 1.0, -1.0).0 > 0.0);
        // Stick up: orbit pitch falls; speed scales.
        let (_, dp) = stick_orbit(0.0, 1.0, 0.5, 2.0, 1.0);
        assert!((dp + 150f32.to_radians()).abs() < 1e-5);
    }

    #[test]
    fn mouse_delta_maps_to_the_orbit_through_stick_orbit() {
        // 100 px right in 1/60 s at sens 1: stick_orbit must turn the view 100 * 0.15 = 15 deg.
        let dt = 1.0 / 60.0;
        let (sx, sy) = mouse_axes(100, 0, dt, 1.0).unwrap();
        assert!(sx > 0.0 && sy == 0.0);
        let (dyaw, _) = stick_orbit(sx, sy, dt, 1.0, 1.0);
        assert!((dyaw + 15f32.to_radians()).abs() < 1e-4);
        // Sensitivity scales it; mouse up (dy < 0) is stick up; mouse down is stick down.
        let (sx2, _) = mouse_axes(100, 0, dt, 0.5).unwrap();
        assert!((sx2 - sx / 2.0).abs() < 1e-5);
        assert!(mouse_axes(0, -10, dt, 1.0).unwrap().1 > 0.0);
        assert!(mouse_axes(0, 10, dt, 1.0).unwrap().1 < 0.0);
        assert!(mouse_axes(-10, 0, dt, 1.0).unwrap().0 < 0.0);
        assert_eq!(mouse_axes(0, 0, dt, 1.0), Some((0.0, 0.0)));
    }

    #[test]
    fn mouse_axes_clamp_and_reject_warps() {
        // A hard flick is clipped to the axis limit, both ways.
        let dt = 0.001;
        assert_eq!(mouse_axes(200, 0, dt, 5.0), Some((MOUSE_AXIS_MAX, 0.0)));
        assert_eq!(mouse_axes(-200, 200, dt, 5.0), Some((-MOUSE_AXIS_MAX, -MOUSE_AXIS_MAX)));
        // A recentering warp or a stale sample is not input.
        assert_eq!(mouse_axes(MOUSE_MAX_JUMP_PX + 1, 0, 0.016, 1.0), None);
        assert_eq!(mouse_axes(0, -(MOUSE_MAX_JUMP_PX + 1), 0.016, 1.0), None);
        assert!(mouse_axes(MOUSE_MAX_JUMP_PX, 0, 0.016, 1.0).is_some());
        assert_eq!(mouse_axes(5, 5, 0.0, 1.0), None);
        assert_eq!(mouse_axes(5, 5, 0.3, 1.0), None);
        assert_eq!(mouse_axes(5, 5, f32::NAN, 1.0), None);
    }

    #[test]
    fn arrow_keys_map_to_full_deflection() {
        assert_eq!(arrow_axes(false, false, false, false), (0.0, 0.0));
        assert_eq!(arrow_axes(false, true, false, false), (1.0, 0.0));
        assert_eq!(arrow_axes(true, false, false, false), (-1.0, 0.0));
        assert_eq!(arrow_axes(false, false, true, false), (0.0, 1.0));
        assert_eq!(arrow_axes(false, false, false, true), (0.0, -1.0));
        assert_eq!(arrow_axes(true, true, true, true), (0.0, 0.0)); // opposite keys cancel
        assert_eq!(arrow_axes(false, true, true, false), (1.0, 1.0));
    }

    #[test]
    fn pad_wins_the_merge_per_axis() {
        assert_eq!(merge_look((0.0, 0.0), (0.4, -0.2)), (0.4, -0.2)); // only the keyboard / mouse
        assert_eq!(merge_look((0.5, 0.0), (0.4, -0.2)), (0.5, -0.2)); // pad x wins, y falls through
        assert_eq!(merge_look((-0.3, 0.9), (1.0, 1.0)), (-0.3, 0.9)); // both pad axes win
        assert_eq!(merge_look((0.0, 0.0), (0.0, 0.0)), (0.0, 0.0));
    }

    #[test]
    fn keyboard_look_off_switch_and_priority() {
        assert_eq!(keyboard_look(false, Some((1.0, 1.0)), (1.0, 1.0)), (0.0, 0.0));
        assert_eq!(keyboard_look(true, None, (1.0, 0.0)), (1.0, 0.0));
        assert_eq!(keyboard_look(true, Some((0.3, 0.0)), (1.0, 1.0)), (0.3, 1.0)); // mouse wins per axis
        assert_eq!(keyboard_look(true, None, (0.0, 0.0)), (0.0, 0.0));
    }

    #[test]
    fn pad_edge_cases() {
        assert_eq!(pad_edge(false, Some(0x0020), 0), (false, false)); // off
        assert_eq!(pad_edge(false, None, 0x0020), (false, false)); // no controller
        assert_eq!(pad_edge(false, Some(0x0020), 0x0020), (true, true)); // press
        assert_eq!(pad_edge(true, Some(0x00A0), 0x0020), (true, false)); // hold
        assert_eq!(pad_edge(true, Some(0x0000), 0x0020), (false, false)); // release
        assert_eq!(pad_edge(true, None, 0x0020), (false, false)); // unplugged
    }

    #[test]
    fn debounce_and_edges() {
        assert!(debounce_ok(None));
        assert!(!debounce_ok(Some(Duration::from_millis(249))));
        assert!(debounce_ok(Some(Duration::from_millis(250))));
        assert!(edge(false, true) && !edge(true, true) && !edge(true, false));
    }

    #[test]
    fn stick_cycle_fires_only_on_the_edge() {
        assert_eq!(stick_edge(0, 0.5), (0, None));
        assert_eq!(stick_edge(0, 0.8), (1, Some(1)));
        assert_eq!(stick_edge(1, 0.9), (1, None));
        assert_eq!(stick_edge(1, 0.0), (0, None));
        assert_eq!(stick_edge(1, -0.9), (-1, Some(-1)));
    }

    #[test]
    fn flick_needs_speed() {
        let mut f = Flick::default();
        // 0.05 rad (2.9 deg) per 0.1 s = slow turn: never reaches 20 deg inside 0.3 s.
        let mut fired = false;
        for i in 0..40 {
            fired |= f.push(i as f32 * 0.1, 0.05).is_some();
        }
        assert!(!fired);
        let mut f = Flick::default();
        assert_eq!(f.push(0.0, 0.2), None);
        assert_eq!(f.push(0.1, 0.2), Some(1)); // 0.4 rad = 23 deg within 0.1 s
        assert_eq!(f.push(0.2, -0.5), None); // sleeping
        assert_eq!(f.push(0.7, -0.5), Some(-1));
    }
}
