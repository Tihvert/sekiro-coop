//! Overlay (ImGui via hudhook, DX11): a status line in the top left corner, notices, the
//! friend's nameplate and the off-screen arrow. There is no menu; settings live in
//! `sekiro_coop_settings.ini` (`settings.rs`).
//!
//! The render thread never touches game memory: it reads a status snapshot the game task
//! publishes and posts the debug hotkeys (`debug_hotkeys = 1`: F8, F9, F11) back through
//! `SHARED`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use hudhook::hooks::dx11::ImguiDx11Hooks;
use hudhook::imgui::{Condition, Context, FontConfig, FontSource, Io, Key, Ui};
use hudhook::windows::Win32::Foundation::HINSTANCE;
use hudhook::{Hudhook, ImguiRenderLoop, MessageFilter, RenderContext};

use crate::peers::PeerId;
use crate::{log, settings, tuning};

/// Game task → overlay.
#[derive(Default, Clone)]
pub struct Status {
    /// Co-op connection state, shown after "Co-op: ".
    pub net: String,
    /// One nameplate per other player shown: world position (feet), name, ping. A peer
    /// without one is hidden (not connected, other area, or dead).
    pub friend_tags: Vec<FriendTag>,
    /// Extra status lines (limbo, rest, boss), shown under the status line.
    pub notice: Option<String>,
    /// Party HP bars (companion), drawn on the left of the screen. Empty = none.
    pub party: Vec<PartyMember>,
}

/// One row of the party HP bar. `state`: 0 = here, 1 = other area / loading, 2 = dead.
#[derive(Clone)]
pub struct PartyMember {
    /// Whose row (head mode hangs it under that peer's nameplate).
    pub id: PeerId,
    pub name: String,
    pub hp: i32,
    pub max_hp: i32,
    pub state: u8,
}

#[derive(Clone, Debug)]
pub struct FriendTag {
    /// Whose nameplate (PLAN-MULTIPLAYER §5.3). The tags come in id order, so stacked
    /// off-screen markers (`edge_stack`) keep a stable order; head mode finds its HP row.
    pub id: PeerId,
    pub pos: [f32; 3],
    pub name: String,
    pub ping_ms: Option<u32>,
}

/// Overlay → game task (consumed once). Only the debug hotkeys post these.
#[derive(Default)]
pub struct Commands {
    /// F8: stop waiting on the death screen and go to the idol (bossrules.rs).
    pub wake: bool,
    /// F9: warp to the friend now (join.rs go_to_friend).
    pub go_to_friend: bool,
    /// F11: one log line with the unix time (ms) to line up what was seen.
    pub mark: bool,
}

#[derive(Default)]
struct Shared {
    status: Status,
    commands: Commands,
}

static SHARED: Mutex<Option<Shared>> = Mutex::new(None);

pub fn publish(status: Status) {
    if let Ok(mut s) = SHARED.lock() {
        s.get_or_insert_with(Shared::default).status = status;
    }
}

/// The nameplates every frame (the rest of the status only every 10 frames:
/// the tag moved in steps, "slideshow", test 2026-10-22).
pub fn publish_tags(tags: Vec<FriendTag>) {
    if let Ok(mut s) = SHARED.lock() {
        s.get_or_insert_with(Shared::default).status.friend_tags = tags;
    }
}

/// One change in who is connected (`presence`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Presence {
    Joined(PeerId, String),
    Left(PeerId, String),
}

/// The connected peers as `presence` last saw them.
static PRESENT: Mutex<Vec<(PeerId, String)>> = Mutex::new(Vec::new());

/// The changes from `before` to `now` (both in id order, names as shown): a new id
/// joined, a missing id left, the same id with another name (a rejoin under another
/// name) is only a join, as v7.5's single "NAME joined".
pub fn presence_diff(before: &[(PeerId, String)], now: &[(PeerId, String)]) -> Vec<Presence> {
    let mut out = Vec::new();
    for (id, name) in before {
        if !now.iter().any(|(n, _)| n == id) {
            out.push(Presence::Left(*id, name.clone()));
        }
    }
    for (id, name) in now {
        if !before.iter().any(|(b, n)| b == id && n == name) {
            out.push(Presence::Joined(*id, name.clone()));
        }
    }
    out
}

/// Game task, every frame: the connected peers now (id order; empty while unlinked) ->
/// the overlay text for whoever joined or left since the last call ("NAME joined" /
/// "NAME left", one line each, as v7.5 for the one friend). The game's notice "... has
/// entered your world." is leave.rs' alone (host, `entered_text`): a second one from here
/// was a duplicate (review finding 6).
pub fn presence(now: &[(PeerId, String)]) -> Option<String> {
    let Ok(mut present) = PRESENT.lock() else { return None };
    let before = std::mem::replace(&mut *present, now.to_vec());
    drop(present);
    let changes = presence_diff(&before, now);
    if changes.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    for c in &changes {
        let text = match c {
            Presence::Joined(_, name) => format!("{name} joined"),
            Presence::Left(_, name) => format!("{name} left"),
        };
        log::line(&format!("session: {text}"));
        lines.push(text);
    }
    Some(lines.join("\n"))
}

pub fn take_commands() -> Commands {
    SHARED
        .lock()
        .ok()
        .and_then(|mut s| s.as_mut().map(|s| std::mem::take(&mut s.commands)))
        .unwrap_or_default()
}

fn post(f: impl FnOnce(&mut Commands)) {
    if let Ok(mut s) = SHARED.lock() {
        f(&mut s.get_or_insert_with(Shared::default).commands);
    }
}

/// Shown in the status line, so both players see at once whether the mod was updated.
/// Bump it with every release together with the first line of `dist/sekiro-coop/OHJE.txt`.
/// Public numbering from 1.0-beta (= internal build 8.62, 2026-10-28); the 8.x numbers live on
/// only in docs/JATKA-TASTA-MULTIPLAYER.md. 1.0.1-beta: SprjTask lookup retried (Nexus "no ini" reports). 1.0.2-beta: clean public build (build-public.cmd). 1.0.3-beta: Steam lobby AccessDenied explained, joiner without a host told how to use UDP.
pub const MOD_VERSION: &str = "1.0.3-beta";

const YELLOW: [f32; 4] = [1.0, 0.85, 0.3, 1.0];
/// Nameplate: full opacity up to this distance, then slowly lighter.
const FADE_FROM_M: f32 = 100.0;
/// Nameplate opacity far away (never hidden by distance).
const MIN_FAR_ALPHA: f32 = 0.85;
/// From this distance the nameplate shows "name  N m".
const SHOW_DIST_FROM_M: f32 = 40.0;

#[derive(Default)]
struct Overlay {
    panel: Panel,
}

/// The 'Mod settings' panel is on screen right now (render thread writes, `message_filter`
/// reads: it takes `&self`).
static PANEL_VISIBLE: AtomicBool = AtomicBool::new(false);

/// The imgui key for a Windows virtual-key code (`ingame_menu_key`); Insert for anything
/// that has no mapping here.
fn vk_to_key(vk: u32) -> Key {
    const F: [Key; 12] = [Key::F1, Key::F2, Key::F3, Key::F4, Key::F5, Key::F6, Key::F7, Key::F8, Key::F9, Key::F10, Key::F11, Key::F12];
    match vk {
        33 => Key::PageUp,
        34 => Key::PageDown,
        35 => Key::End,
        36 => Key::Home,
        46 => Key::Delete,
        112..=123 => F[(vk - 112) as usize],
        192 => Key::GraveAccent,
        _ => Key::Insert,
    }
}

fn vk_name(vk: u32) -> String {
    match vk {
        33 => "PgUp".into(),
        34 => "PgDn".into(),
        35 => "End".into(),
        36 => "Home".into(),
        46 => "Delete".into(),
        112..=123 => format!("F{}", vk - 111),
        192 => "`".into(),
        _ => "Insert".into(),
    }
}

/// Draft of the 'Mod settings' panel (render thread only). Edited locally; the Apply button
/// posts it to the game task (`menu::post_edit`), which applies it and writes the ini.
#[derive(Default)]
struct Panel {
    draft: crate::menu::Values,
    /// What the draft was last synced from: while `draft == synced` nothing is being edited
    /// and the draft follows the live values.
    synced: crate::menu::Values,
    was_visible: bool,
    collapsed: bool,
    /// Controller navigation (v8.32a): the selected row (0..6) and the pure navigator.
    selected: usize,
    nav: crate::menu::PadNav,
}

/// Milliseconds since the first call (clock of the pad navigator).
fn nav_now_ms() -> u64 {
    static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    T0.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

impl Panel {
    fn draw(&mut self, ui: &Ui) {
        use crate::menu;
        let (role, mine, host, open) = menu::snapshot();
        let visible = tuning::ingame_menu() && open;
        PANEL_VISIBLE.store(visible, Ordering::Relaxed);
        if !visible {
            self.was_visible = false;
            return;
        }
        let editable = menu::can_edit(role);
        // A client sees the host's values (its own ini does not apply to it).
        let source = if editable { mine } else { host.unwrap_or(mine) };
        if !self.was_visible || self.draft == self.synced {
            self.draft = source;
        }
        self.synced = source;
        self.was_visible = true;

        let vk = tuning::ingame_menu_key();
        if ui.is_key_pressed_no_repeat(vk_to_key(vk)) {
            self.collapsed = !self.collapsed;
        }
        let [dw, dh] = ui.io().display_size;
        let width = (dw * 0.17).clamp(260.0, 520.0);
        let mut apply = false;
        // Controller: right stick / triggers / R3 only (render thread; XInputGetState only).
        let pad_on = editable && !self.collapsed && tuning::ingame_menu_pad();
        if pad_on {
            let now = nav_now_ms();
            for a in self.nav.step(crate::spectate_input::pad_state(), now) {
                let (sel, d, ap) = menu::apply_nav(self.selected, self.draft, a);
                self.selected = sel;
                self.draft = d;
                apply |= ap;
            }
        } else {
            self.nav = menu::PadNav::default();
        }
        let sel = if pad_on { Some(self.selected) } else { None };
        let mark = |ui: &Ui, row: usize| {
            if let Some(s) = sel {
                if s == row {
                    ui.text_colored(YELLOW, ">");
                } else {
                    ui.text(" ");
                }
                ui.same_line();
            }
        };
        ui.window("Mod settings")
            .position([dw - 12.0, dh * 0.5], Condition::Always)
            .position_pivot([1.0, 0.5])
            .always_auto_resize(true)
            .resizable(false)
            .movable(false)
            .collapsible(false)
            .focus_on_appearing(false)
            .save_settings(false)
            .bg_alpha(0.85)
            .build(|| {
                if self.collapsed {
                    ui.text(format!("[{}] expand", vk_name(vk)));
                    return;
                }
                if editable {
                    ui.text(format!("Host settings  [{}] collapse", vk_name(vk)));
                    if pad_on {
                        ui.text_wrapped("Pad: right stick up/down = row, left/right = value (LB/RB = x5), RT = Apply, R3 = toggle");
                    }
                } else {
                    ui.text_colored(YELLOW, "Set by the host");
                    if host.is_none() {
                        ui.text("Waiting for the host's values...");
                    }
                }
                ui.separator();
                let d = &mut self.draft;
                ui.disabled(!editable, || {
                    mark(ui, 0);
                    ui.set_next_item_width(width * 0.55);
                    let mut v = d.enemy_hp as i32;
                    ui.slider("Enemy HP %", 0, 1000, &mut v);
                    d.enemy_hp = menu::clamp_pct(v as i64);
                    mark(ui, 1);
                    ui.set_next_item_width(width * 0.55);
                    let mut v = d.enemy_posture as i32;
                    ui.slider("Enemy posture %", 0, 1000, &mut v);
                    d.enemy_posture = menu::clamp_pct(v as i64);
                    mark(ui, 2);
                    ui.set_next_item_width(width * 0.55);
                    let mut v = d.boss_hp as i32;
                    ui.slider("Boss HP %", 0, 1000, &mut v);
                    d.boss_hp = menu::clamp_pct(v as i64);
                    mark(ui, 3);
                    ui.set_next_item_width(width * 0.55);
                    let mut v = d.boss_posture as i32;
                    ui.slider("Boss posture %", 0, 1000, &mut v);
                    d.boss_posture = menu::clamp_pct(v as i64);
                    mark(ui, 4);
                    ui.checkbox("Death debuff (-10 % max HP/posture per death)", &mut d.death_debuffs);
                    mark(ui, 5);
                    if editable && ui.button("Apply") {
                        apply = true;
                    }
                });
                if editable {
                    ui.text_disabled("Percent added per extra player. Ctrl+click a slider to type.");
                    let r = menu::save_result();
                    if !r.is_empty() {
                        ui.text_wrapped(r);
                    }
                }
            });
        if apply {
            menu::post_edit(self.draft);
        }
    }
}

/// Render camera (right, up, forward, position rows; vertical FOV), None on any failed read.
/// Spectate lock-on (v8.10): a white dot in a dark ring on the locked target, like the
/// game's own lock-on marker while alive.
fn draw_lock_mark(ui: &Ui) {
    let Some(p) = crate::spectate_cam::lock_mark() else { return };
    let Some((m, fov)) = read_camera() else { return };
    let [w, h] = ui.io().display_size;
    if w < 1.0 || h < 1.0 {
        return;
    }
    let Some([sx, sy]) = project(&m, fov, [w, h], p) else { return };
    let dl = ui.get_foreground_draw_list();
    dl.add_circle([sx, sy], 9.0, [0.0, 0.0, 0.0, 0.55]).thickness(3.0).num_segments(24).build();
    dl.add_circle([sx, sy], 5.5, [1.0, 1.0, 1.0, 0.95]).filled(true).num_segments(20).build();
    dl.add_circle([sx, sy], 5.5, [0.85, 0.15, 0.1, 0.9]).thickness(1.5).num_segments(20).build();
}

/// World point -> screen pixel with the render camera (None behind it or off screen).
fn project(m: &[[f32; 4]; 4], fov: f32, [w, h]: [f32; 2], p: [f32; 3]) -> Option<[f32; 2]> {
    let aspect = w / h;
    let vfov = if aspect < 16.0 / 9.0 { 2.0 * ((fov * 0.5).tan() * (16.0 / 9.0) / aspect).atan() } else { fov };
    let d = [p[0] - m[3][0], p[1] - m[3][1], p[2] - m[3][2]];
    let dot = |r: &[f32; 4]| d[0] * r[0] + d[1] * r[1] + d[2] * r[2];
    let (x, y, z) = (dot(&m[0]), dot(&m[1]), dot(&m[2]));
    if z < 0.3 {
        return None;
    }
    let t = (vfov * 0.5).tan();
    let sx = (0.5 + 0.5 * x / (z * t * aspect)) * w;
    let sy = (0.5 - 0.5 * y / (z * t)) * h;
    (sx.is_finite() && sy.is_finite() && (0.0..=w).contains(&sx) && (0.0..=h).contains(&sy)).then_some([sx, sy])
}

fn read_camera() -> Option<([[f32; 4]; 4], f32)> {
    use crate::game::{module_base, read, read_ptr};
    let base = module_base();
    if base == 0 {
        return None;
    }
    let field_area = read_ptr(base + 0x3D5C0A0).filter(|&p| p != 0)?;
    let game_rend = read_ptr(field_area + 0x20).filter(|&p| p != 0)?;
    let cam = read_ptr(game_rend + 0x18).filter(|&p| p != 0)?;
    let rows: [[f32; 4]; 4] = read(cam + 0x10)?;
    let fov: f32 = read(cam + 0x50)?;
    if rows.iter().flatten().any(|v| !v.is_finite()) || !fov.is_finite() || !(0.05..=3.0).contains(&fov) {
        return None;
    }
    Some((rows, fov))
}


impl Overlay {
    /// Party HP list for `party_hp_position` left / top (head mode is drawn by `draw_tag`).
    /// "L2 R2 Page n/m" under the Co-op Settings page: two drawn trigger icons and the page number (the game has
    /// no picture of the triggers for custom pages, and the key guide row of a page is fixed).
    fn draw_page_hint(&self, ui: &Ui) {
        let Some((cur, total)) = crate::optmenu::page_hint() else { return };
        let [w, h] = ui.io().display_size;
        if w < 1.0 || h < 1.0 {
            return;
        }
        let k = h / 1080.0;
        let dl = ui.get_foreground_draw_list();
        let (bw, bh) = (58.0 * k, 30.0 * k);
        let y = h * 0.935;
        let mut x = w * 0.58;
        for name in ["L2", "R2"] {
            let (x1, y1) = (x + bw, y + bh);
            dl.add_rect([x, y], [x1, y1], [0.08, 0.08, 0.08, 0.9]).filled(true).rounding(bh * 0.45).build();
            dl.add_rect([x, y], [x1, y1], [0.85, 0.85, 0.85, 0.95]).rounding(bh * 0.45).thickness(2.0 * k).build();
            let tw = ui.calc_text_size(name);
            dl.add_text([x + (bw - tw[0]) * 0.5, y + (bh - tw[1]) * 0.5], [1.0, 1.0, 1.0, 1.0], name);
            x = x1 + 10.0 * k;
        }
        let label = format!("Page {cur}/{total}");
        let tw = ui.calc_text_size(&label);
        dl.add_text([x + 6.0 * k, y + (bh - tw[1]) * 0.5], [1.0, 1.0, 1.0, 0.95], &label);
    }

    fn draw_party(&self, ui: &Ui, status: &Status) {
        let pos = settings::party_hp_position();
        if !settings::party_hp_bars() || status.party.is_empty() || pos == settings::PARTY_POS_HEAD {
            return;
        }
        let [w, h] = ui.io().display_size;
        if w < 1.0 || h < 1.0 {
            return;
        }
        let content = settings::party_hp_content();
        let k = h / 1080.0 * settings::party_hp_scale() as f32 / 100.0;
        let (x, y0) = row_origin(pos, w, h);
        let (bar_w, bar_h) = (220.0 * k, (9.0 * k).max(2.0));
        let dl = ui.get_foreground_draw_list();
        let text_h = ui.calc_text_size("Ag")[1];
        let show_name = content == settings::PARTY_CONTENT_NAME_HP;
        let row_h = if show_name { 40.0 * k.max(h / 1080.0) } else { bar_h + 14.0 * k };
        for (i, m) in status.party.iter().enumerate() {
            let y = y0 + i as f32 * row_h;
            let mut by = y;
            if show_name {
                dl.add_text([x, y], [1.0, 1.0, 1.0, 0.95], &m.name);
                if let Some(t) = state_tag(m.state) {
                    let nw = ui.calc_text_size(&m.name)[0];
                    dl.add_text([x + nw + 6.0 * k, y], [0.6, 0.6, 0.6, 0.9], t);
                }
                by = y + text_h + 2.0 * k;
            }
            draw_bar(&dl, [x, by, x + bar_w, by + bar_h], m);
            // Right of the bar: "hp / max", and the state when the name is not shown.
            let mut side = String::new();
            if content == settings::PARTY_CONTENT_HP_NUMBERS {
                side = hp_numbers(m);
            }
            if !show_name && let Some(t) = state_tag(m.state) {
                if !side.is_empty() {
                    side.push(' ');
                }
                side.push_str(t);
            }
            if !side.is_empty() {
                let th = ui.calc_text_size(&side)[1];
                dl.add_text([x + bar_w + 6.0 * k, by + bar_h * 0.5 - th * 0.5], [0.9, 0.9, 0.9, 0.9], &side);
            }
        }
    }

    /// `party_hp_position = head`: the bar under the nameplate plate (`plate` = its rect).
    fn draw_head_bar(ui: &Ui, status: &Status, tag: &FriendTag, plate: [f32; 4], h: f32) {
        if !settings::party_hp_bars() || settings::party_hp_position() != settings::PARTY_POS_HEAD {
            return;
        }
        // The bar of the peer this nameplate belongs to (each peer its own).
        let Some(m) = member_of(&status.party, tag.id) else { return };
        let rect = head_bar_rect(plate, h, settings::party_hp_scale());
        let dl = ui.get_foreground_draw_list();
        draw_bar(&dl, rect, m);
        // The state under the nameplate too ("(in cutscene)" ...), when the row has one.
        if settings::party_hp_content() != settings::PARTY_CONTENT_HP_NUMBERS
            && let Some(t) = state_tag(m.state)
        {
            let th = ui.calc_text_size(t)[1];
            let k = h / 1080.0 * settings::party_hp_scale() as f32 / 100.0;
            dl.add_text([rect[2] + 5.0 * k, (rect[1] + rect[3]) * 0.5 - th * 0.5], [0.6, 0.6, 0.6, 0.9], t);
        }
        if settings::party_hp_content() == settings::PARTY_CONTENT_HP_NUMBERS {
            let text = hp_numbers(m);
            let th = ui.calc_text_size(&text)[1];
            let k = h / 1080.0 * settings::party_hp_scale() as f32 / 100.0;
            dl.add_text([rect[2] + 5.0 * k, (rect[1] + rect[3]) * 0.5 - th * 0.5], [0.9, 0.9, 0.9, 0.9], &text);
        }
    }

    /// Every nameplate: projects each head point with the render camera.
    fn draw_tags(&self, ui: &Ui, status: &Status) {
        // overhead_player_display: 0 name + ping, 1 name only, 2 nothing (arrow included).
        let mode = settings::overhead_display();
        if mode >= 2 || status.friend_tags.is_empty() {
            return;
        }
        let Some((m, fov)) = read_camera() else { return };
        let [w, h] = ui.io().display_size;
        if w < 1.0 || h < 1.0 {
            return;
        }
        // Off-screen markers already drawn this frame (their plates), so the next peer's
        // marker that points the same way is stacked beside them instead of on top.
        let mut placed: Vec<[f32; 4]> = Vec::new();
        for tag in &status.friend_tags {
            Self::draw_tag(ui, status, tag, mode, &m, fov, [w, h], &mut placed);
        }
    }

    /// One nameplate, or its edge marker when off screen.
    #[allow(clippy::too_many_arguments)]
    fn draw_tag(ui: &Ui, status: &Status, tag: &FriendTag, mode: u8, m: &[[f32; 4]; 4], fov: f32, [w, h]: [f32; 2], placed: &mut Vec<[f32; 4]>) {
        let aspect = w / h;
        // The game keeps the 16:9 horizontal FOV on narrower screens (0xF01260).
        let vfov = if aspect < 16.0 / 9.0 {
            2.0 * ((fov * 0.5).tan() * (16.0 / 9.0) / aspect).atan()
        } else {
            fov
        };
        let p = [tag.pos[0], tag.pos[1] + 1.9, tag.pos[2]];
        let d = [p[0] - m[3][0], p[1] - m[3][1], p[2] - m[3][2]];
        let dot = |r: &[f32; 4]| d[0] * r[0] + d[1] * r[1] + d[2] * r[2];
        let (x, y, z) = (dot(&m[0]), dot(&m[1]), dot(&m[2]));
        let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let t = (vfov * 0.5).tan();
        if z < 0.3 {
            // Behind the camera: point the edge marker the way to turn.
            let v = if x.abs() + y.abs() < 1e-3 { [0.0, 1.0] } else { [x, -y] };
            if let Some(plate) = draw_edge_marker(ui, tag, v, dist, w, h, placed) {
                Self::draw_head_bar(ui, status, tag, plate, h);
            }
            return;
        }
        let sx = (0.5 + 0.5 * x / (z * t * aspect)) * w;
        let sy = (0.5 - 0.5 * y / (z * t)) * h;
        if !(sx.is_finite() && sy.is_finite()) {
            return;
        }
        if sx < 0.0 || sx > w || sy < 0.0 || sy > h {
            if let Some(plate) = draw_edge_marker(ui, tag, [sx - w * 0.5, sy - h * 0.5], dist, w, h, placed) {
                Self::draw_head_bar(ui, status, tag, plate, h);
            }
            return;
        }
        // Visible at any distance in our map block (owner 2026-10-24, like ERSC; it faded out
        // between 100 and 150 m before). The text is drawn at the font's fixed pixel size, so
        // it never shrinks with distance; far away it only gets a little lighter, never below
        // 85 %, and shows the distance next to the name.
        let alpha = if dist <= FADE_FROM_M { 1.0 } else { (1.0 - (dist - FADE_FROM_M) / 1000.0).clamp(MIN_FAR_ALPHA, 1.0) };
        let far_name;
        let name: &str = if dist >= SHOW_DIST_FROM_M {
            far_name = format!("{}  {:.0} m", tag.name, dist);
            &far_name
        } else {
            &tag.name
        };
        let dl = ui.get_foreground_draw_list();
        let bg_col = [0.0, 0.0, 0.0, 0.6 * alpha];
        let name_col = [1.0, 0.92, 0.45, alpha];
        let ping_col = [0.8, 0.8, 0.8, 0.8 * alpha];
        let ping = match tag.ping_ms {
            Some(ms) => format!("ping {ms} ms"),
            None => "ping -".to_string(),
        };
        let line_h = ui.calc_text_size("Ag")[1];
        let name_w = ui.calc_text_size(name)[0];
        let ping_w = if mode == 0 { ui.calc_text_size(&ping)[0] } else { 0.0 };
        // Dark rounded plate behind the text keeps it readable on any background.
        let pad = 5.0;
        let half_w = name_w.max(ping_w) * 0.5 + pad;
        let below = if mode == 0 { line_h } else { 0.0 };
        dl.add_rect([sx - half_w, sy - line_h - pad], [sx + half_w, sy + below + pad], bg_col)
            .filled(true)
            .rounding(4.0)
            .build();
        // Name above the point, ping line just under it.
        dl.add_text([sx - name_w * 0.5, sy - line_h], name_col, name);
        if mode == 0 {
            dl.add_text([sx - ping_w * 0.5, sy], ping_col, &ping);
        }
        // imgui allows one foreground DrawListMut at a time and the bar takes its own
        // (live 2026-10-24 v8.2: panic "already loaded" the moment a companion appeared).
        drop(dl);
        Self::draw_head_bar(ui, status, tag, [sx - half_w, sy - line_h - pad, sx + half_w, sy + below + pad], h);
    }
}

impl ImguiRenderLoop for Overlay {
    // ImGui's built-in 13 px bitmap font looked pixelated on a friend's screen
    // (2026-10-22). Use Windows' Segoe UI, sized to the screen; fall back to
    // the built-in font if the file is missing.
    fn initialize<'a>(&'a mut self, ctx: &mut Context, _rc: &'a mut dyn RenderContext) {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        let Ok(bytes) = std::fs::read(format!(r"{windir}\Fonts\segoeui.ttf")) else {
            log::line("ui: segoeui.ttf not found, using built-in font");
            return;
        };
        let data: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        let height = ctx.io().display_size[1];
        let size = if height > 0.0 { (height / 1080.0 * 20.0).clamp(16.0, 40.0) } else { 20.0 };
        ctx.fonts().add_font(&[FontSource::TtfData {
            data,
            size_pixels: size,
            config: Some(FontConfig { oversample_h: 2, ..FontConfig::default() }),
        }]);
        log::line(&format!("ui: font Segoe UI {size:.0} px"));
    }

    /// The mouse cursor is drawn by ImGui only while the 'Mod settings' panel is up (it is
    /// not known whether the game shows its own cursor in the System menu; if it does, this
    /// draws a second one).
    fn before_render<'a>(&'a mut self, ctx: &mut Context, _rc: &'a mut dyn RenderContext) {
        ctx.io_mut().mouse_draw_cursor = PANEL_VISIBLE.load(Ordering::Relaxed);
    }

    /// Mouse messages are kept from the game only while the panel is up and the pointer is
    /// over it (`want_capture_mouse`); the keyboard is never filtered.
    fn message_filter(&self, io: &Io) -> MessageFilter {
        if PANEL_VISIBLE.load(Ordering::Relaxed) && io.want_capture_mouse {
            MessageFilter::InputMouse
        } else {
            MessageFilter::empty()
        }
    }

    fn render(&mut self, ui: &mut Ui) {
        // Debug hotkeys (settings: debug_hotkeys = 1).
        if tuning::debug_hotkeys() {
            if ui.is_key_pressed_no_repeat(Key::F8) {
                post(|c| c.wake = true);
            }
            if ui.is_key_pressed_no_repeat(Key::F9) {
                post(|c| c.go_to_friend = true);
            }
            if ui.is_key_pressed_no_repeat(Key::F11) {
                post(|c| c.mark = true);
            }
        }
        let status = SHARED
            .lock()
            .ok()
            .and_then(|s| s.as_ref().map(|s| s.status.clone()))
            .unwrap_or_default();

        self.draw_tags(ui, &status);
        draw_lock_mark(ui);
        self.draw_party(ui, &status);
        self.draw_page_hint(ui);
        self.panel.draw(ui);

        if !settings::show_status_overlay() {
            return;
        }
        ui.window("##coop-status")
            .position([10.0, 10.0], Condition::Always)
            .no_decoration()
            .always_auto_resize(true)
            .bg_alpha(0.35)
            .no_inputs()
            .build(|| {
                ui.text(format!("Co-op v{MOD_VERSION}: {}", status.net));
                if let Some(n) = &status.notice {
                    ui.text_colored(YELLOW, n);
                }
            });
    }
}

/// Installs the DX11 overlay. Call once from a background thread.
pub fn install(hmodule: usize) {
    let result = Hudhook::builder()
        .with::<ImguiDx11Hooks>(Overlay::default())
        .with_hmodule(HINSTANCE(hmodule as *mut _))
        .build()
        .apply();
    match result {
        Ok(()) => log::line("ui: overlay installed"),
        Err(e) => log::line(&format!("ui: overlay failed: {e:?}")),
    }
}

/// Friend off screen: name and distance at the screen edge in the friend's
/// direction (`v` = direction from the screen centre, screen axes), with a
/// small triangle pointing outwards.
/// Returns the plate rect `[x0, y0, x1, y1]` (the party HP bar hangs under it).
fn draw_edge_marker(ui: &Ui, tag: &FriendTag, v: [f32; 2], dist: f32, w: f32, h: f32, placed: &mut Vec<[f32; 4]>) -> Option<[f32; 4]> {
    let len = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if !len.is_finite() || len < 1e-3 {
        return None;
    }
    let dir = [v[0] / len, v[1] / len];
    let margin = 70.0;
    let (cx, cy) = (w * 0.5, h * 0.5);
    let kx = if dir[0].abs() > 1e-4 { (cx - margin) / dir[0].abs() } else { f32::MAX };
    let ky = if dir[1].abs() > 1e-4 { (cy - margin) / dir[1].abs() } else { f32::MAX };
    let k = kx.min(ky);
    let (px, py) = (cx + dir[0] * k, cy + dir[1] * k);
    let text = format!("{}  {:.0} m", tag.name, dist);
    let tw = ui.calc_text_size(&text)[0];
    let lh = ui.calc_text_size("Ag")[1];
    let dl = ui.get_foreground_draw_list();
    let pad = 5.0;
    // On the left / right edge the plates stack vertically, on the top / bottom sideways.
    let half = [tw * 0.5 + pad, lh * 0.5 + pad];
    let [px, py] = edge_stack([px, py], half, kx < ky, placed);
    placed.push([px - half[0], py - half[1], px + half[0], py + half[1]]);
    dl.add_rect([px - tw * 0.5 - pad, py - lh * 0.5 - pad], [px + tw * 0.5 + pad, py + lh * 0.5 + pad], [0.0, 0.0, 0.0, 0.6])
        .filled(true)
        .rounding(4.0)
        .build();
    dl.add_text([px - tw * 0.5, py - lh * 0.5], [1.0, 0.92, 0.45, 1.0], &text);
    // Triangle just outside the plate, pointing towards the friend.
    let tip = [px + dir[0] * (margin - 12.0), py + dir[1] * (margin - 12.0)];
    let base = [px + dir[0] * (margin - 30.0), py + dir[1] * (margin - 30.0)];
    let side = [-dir[1] * 9.0, dir[0] * 9.0];
    dl.add_triangle(tip, [base[0] + side[0], base[1] + side[1]], [base[0] - side[0], base[1] - side[1]], [1.0, 0.92, 0.45, 0.9])
        .filled(true)
        .build();
    Some([px - tw * 0.5 - pad, py - lh * 0.5 - pad, px + tw * 0.5 + pad, py + lh * 0.5 + pad])
}

/// Head mode: the party row of the peer `id` (None: no bar for that nameplate).
fn member_of(party: &[PartyMember], id: PeerId) -> Option<&PartyMember> {
    party.iter().find(|m| m.id == id)
}

/// "(away)" / "(dead)" / "(in cutscene)" for a party member state, None when here.
fn state_tag(state: u8) -> Option<&'static str> {
    match state {
        1 => Some("(away)"),
        2 => Some("(dead)"),
        3 => Some("(in cutscene)"),
        _ => None,
    }
}

/// "hp / max" (hp never negative).
fn hp_numbers(m: &PartyMember) -> String {
    format!("{} / {}", m.hp.max(0), m.max_hp.max(0))
}

/// Filled part of the bar, 0..1. A dead member (or unknown max) is an empty bar.
fn hp_frac(m: &PartyMember) -> f32 {
    if m.max_hp > 0 && m.state != 2 { (m.hp as f32 / m.max_hp as f32).clamp(0.0, 1.0) } else { 0.0 }
}

/// Top-left of the left / top party list (`party_hp_position`), x 2 % of the width.
fn row_origin(pos: u8, w: f32, h: f32) -> (f32, f32) {
    let y = match pos {
        settings::PARTY_POS_TOP => 0.06,
        settings::PARTY_POS_LEFT => 0.30,
        _ => 0.30,
    };
    (w * 0.02, h * y)
}

/// Head-mode bar under the nameplate plate `[x0, y0, x1, y1]`: as wide as the plate (at
/// least 90 px), 5 px high, 2 px below it, centred; sizes scale with the screen height
/// (1080 = 1) and `scale_pct`.
fn head_bar_rect(plate: [f32; 4], screen_h: f32, scale_pct: u32) -> [f32; 4] {
    let k = screen_h / 1080.0 * scale_pct as f32 / 100.0;
    let width = (plate[2] - plate[0]).max(90.0 * k);
    let cx = (plate[0] + plate[2]) * 0.5;
    let top = plate[3] + 2.0 * k;
    [cx - width * 0.5, top, cx + width * 0.5, top + (5.0 * k).max(2.0)]
}

/// One HP bar in `rect`: dark back, red fill (faded when the member is in another area).
fn draw_bar(dl: &hudhook::imgui::DrawListMut<'_>, rect: [f32; 4], m: &PartyMember) {
    let [x0, y0, x1, y1] = rect;
    dl.add_rect([x0, y0], [x1, y1], [0.0, 0.0, 0.0, 0.6]).filled(true).build();
    let frac = hp_frac(m);
    if frac > 0.0 {
        let a = if m.state == 1 { 0.35 } else { 0.9 };
        dl.add_rect([x0, y0], [x0 + (x1 - x0) * frac, y1], [0.72, 0.10, 0.10, a]).filled(true).build();
    }
    dl.add_rect([x0, y0], [x1, y1], [0.05, 0.05, 0.05, 0.9]).build();
}

#[cfg(test)]
mod party_bar_tests {
    use super::*;

    fn member(hp: i32, max_hp: i32, state: u8) -> PartyMember {
        PartyMember { id: PeerId(1), name: "x".into(), hp, max_hp, state }
    }

    #[test]
    fn head_bar_is_centred_under_the_plate() {
        // 1080p, 100 %: plate 200 wide -> bar 200 wide, 5 high, 2 below.
        let r = head_bar_rect([400.0, 300.0, 600.0, 340.0], 1080.0, 100);
        assert_eq!(r, [400.0, 342.0, 600.0, 347.0]);
    }

    #[test]
    fn head_bar_has_a_minimum_width_and_scales() {
        // Narrow plate (60 px): min 90 px, centred on x = 130.
        let r = head_bar_rect([100.0, 0.0, 160.0, 20.0], 1080.0, 100);
        assert_eq!((r[0], r[2]), (85.0, 175.0));
        // 540p halves the minimum width (45) and height; 200 % doubles them back.
        let half = head_bar_rect([100.0, 0.0, 160.0, 20.0], 540.0, 100);
        assert_eq!(half[2] - half[0], 60.0);
        let narrow = head_bar_rect([100.0, 0.0, 130.0, 20.0], 540.0, 100);
        assert_eq!(narrow[2] - narrow[0], 45.0);
        let big = head_bar_rect([100.0, 0.0, 160.0, 20.0], 540.0, 200);
        assert_eq!((big[2] - big[0], big[3] - big[1]), (90.0, 5.0));
        // Never thinner than 2 px.
        let tiny = head_bar_rect([0.0, 0.0, 10.0, 10.0], 540.0, 50);
        assert_eq!(tiny[3] - tiny[1], 2.0);
    }

    #[test]
    fn row_origin_left_and_top() {
        assert_eq!(row_origin(settings::PARTY_POS_LEFT, 1000.0, 1000.0), (20.0, 300.0));
        let (x, y) = row_origin(settings::PARTY_POS_TOP, 1000.0, 1000.0);
        assert_eq!(x, 20.0);
        assert!((y - 60.0).abs() < 1e-3);
    }

    #[test]
    fn hp_fraction_and_numbers() {
        assert_eq!(hp_frac(&member(50, 200, 0)), 0.25);
        assert_eq!(hp_frac(&member(500, 200, 0)), 1.0);
        assert_eq!(hp_frac(&member(-5, 200, 0)), 0.0);
        assert_eq!(hp_frac(&member(50, 0, 0)), 0.0);
        assert_eq!(hp_frac(&member(50, 200, 2)), 0.0, "dead = empty bar");
        assert_eq!(hp_numbers(&member(-3, 200, 0)), "0 / 200");
        assert_eq!(state_tag(1), Some("(away)"));
        assert_eq!(state_tag(0), None);
        assert_eq!(state_tag(3), Some("(in cutscene)"));
    }

    #[test]
    fn head_mode_finds_each_peers_own_row() {
        let mut a = member(10, 100, 0);
        let mut b = member(70, 100, 0);
        a.id = PeerId(1);
        b.id = PeerId(3);
        let party = [a, b];
        assert_eq!(member_of(&party, PeerId(3)).map(|m| m.hp), Some(70));
        assert_eq!(member_of(&party, PeerId(1)).map(|m| m.hp), Some(10));
        // A nameplate without a row (no snapshot yet) gets no bar, never another peer's.
        assert!(member_of(&party, PeerId(2)).is_none());
    }
}

/// Where an off-screen marker plate (centre `p`, half size `half`) goes so it overlaps
/// none of the plates already `placed` this frame (`[x0, y0, x1, y1]`): moved along the
/// screen edge (`vertical`: up / down) one plate at a time, alternating sides. The first
/// marker, or one with room, stays where it is.
fn edge_stack(p: [f32; 2], half: [f32; 2], vertical: bool, placed: &[[f32; 4]]) -> [f32; 2] {
    let gap = 4.0;
    let step = if vertical { 2.0 * half[1] + gap } else { 2.0 * half[0] + gap };
    let overlaps = |c: [f32; 2]| {
        placed.iter().any(|r| c[0] - half[0] < r[2] && c[0] + half[0] > r[0] && c[1] - half[1] < r[3] && c[1] + half[1] > r[1])
    };
    for k in 0..(2 * crate::peers::MAX_PLAYERS as i32) {
        // 0, +1, -1, +2, -2, ...
        let n = if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) } as f32;
        let c = if vertical { [p[0], p[1] + n * step] } else { [p[0] + n * step, p[1]] };
        if !overlaps(c) {
            return c;
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[(u8, &str)]) -> Vec<(PeerId, String)> {
        v.iter().map(|(s, n)| (PeerId(*s), n.to_string())).collect()
    }

    #[test]
    fn presence_diff_names_who_joined_and_left() {
        let a = names(&[(0, "Host")]);
        let b = names(&[(0, "Host"), (2, "Bea")]);
        assert_eq!(presence_diff(&[], &a), vec![Presence::Joined(PeerId(0), "Host".into())]);
        assert_eq!(presence_diff(&a, &b), vec![Presence::Joined(PeerId(2), "Bea".into())]);
        assert_eq!(presence_diff(&b, &a), vec![Presence::Left(PeerId(2), "Bea".into())]);
        assert!(presence_diff(&b, &b).is_empty());
        // The host link drops: everyone leaves at once, in id order.
        assert_eq!(
            presence_diff(&b, &[]),
            vec![Presence::Left(PeerId(0), "Host".into()), Presence::Left(PeerId(2), "Bea".into())]
        );
        // Same slot, other name (v7.5: only "NEW joined").
        assert_eq!(presence_diff(&a, &names(&[(0, "Other")])), vec![Presence::Joined(PeerId(0), "Other".into())]);
        // A slot reused by someone else in the same frame another left.
        assert_eq!(
            presence_diff(&names(&[(1, "Ann")]), &names(&[(2, "Bea")])),
            vec![Presence::Left(PeerId(1), "Ann".into()), Presence::Joined(PeerId(2), "Bea".into())]
        );
    }

    #[test]
    fn edge_markers_stack_instead_of_overlapping() {
        let half = [50.0, 12.0];
        let mut placed = Vec::new();
        let first = edge_stack([70.0, 300.0], half, true, &placed);
        assert_eq!(first, [70.0, 300.0]);
        placed.push([first[0] - half[0], first[1] - half[1], first[0] + half[0], first[1] + half[1]]);
        let second = edge_stack([70.0, 300.0], half, true, &placed);
        assert_eq!(second, [70.0, 328.0]);
        placed.push([second[0] - half[0], second[1] - half[1], second[0] + half[0], second[1] + half[1]]);
        let third = edge_stack([70.0, 300.0], half, true, &placed);
        assert_eq!(third, [70.0, 272.0]);
        // Far apart: unchanged.
        assert_eq!(edge_stack([70.0, 600.0], half, true, &placed), [70.0, 600.0]);
        // Top edge: sideways.
        assert_eq!(edge_stack([first[0] + 10.0, 300.0], half, false, &placed[..1]), [first[0] + 10.0 + 104.0, 300.0]);
    }
}
