//! Settings file `sekiro_coop_settings.ini`, next to the mod DLL (like Elden Ring Seamless
//! Co-op's `ersc_settings.ini`). Written with comments on the first run; read at start-up
//! and re-read when its modification time changes (checked about every 2 s).
//!
//! Applied live: everything except the network keys, which restart the connection (the
//! game thread drops its `Net` and opens a new one, `take_net_changed`). `observe` is read
//! at game start only (the hooks cannot be removed again).
//!
//! Layout: `password` and `player_name` on top, then [GAMEPLAY], [DISPLAY], [ADVANCED]
//! (transport, UDP, `auto_connect`) and [DEBUG]. Keys are found wherever they are (the
//! sections are only for the reader), so older files with a [NETWORK] section keep
//! working; unknown keys such as the old `role` are ignored.
//!
//! The old `%LOCALAPPDATA%\sekiro-coop\coop.ini` is read once, only to carry the password,
//! name and host address over when the new file does not exist yet. It is not used after that.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering::Relaxed};
use std::time::{Duration, Instant, SystemTime};

use crate::{enemies, fps, items, log, tuning};

pub const FILE_NAME: &str = "sekiro_coop_settings.ini";
const JOIN_CODE_FILE: &str = "udp_join_code.txt";
const CHECK_EVERY: Duration = Duration::from_secs(2);
const DEFAULT_PORT: u16 = 28100;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleExW(flags: u32, addr: *const u16, module: *mut *mut std::ffi::c_void) -> i32;
    fn GetModuleFileNameW(module: *mut std::ffi::c_void, buf: *mut u16, len: u32) -> u32;
}
const FROM_ADDRESS_UNCHANGED: u32 = 0x4 | 0x2;

/// Path of this DLL (GetModuleHandleExW on an address inside it).
pub fn own_dll_path() -> Option<PathBuf> {
    let mut module: *mut std::ffi::c_void = std::ptr::null_mut();
    let here = own_dll_path as *const u16;
    if unsafe { GetModuleHandleExW(FROM_ADDRESS_UNCHANGED, here, &mut module) } == 0 || module.is_null() {
        return None;
    }
    let mut buf = [0u16; 520];
    let n = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    (n > 0 && n < buf.len()).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

fn legacy_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("sekiro-coop")
}

/// The folder of the DLL (so a friend's `dist` folder holds its own settings); the
/// old `%LOCALAPPDATA%\sekiro-coop` only if the DLL path cannot be found.
fn dir() -> PathBuf {
    own_dll_path().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_else(legacy_dir)
}

pub fn path() -> PathBuf {
    dir().join(FILE_NAME)
}

/// Where a UDP host writes the join code once the router port is open.
pub fn join_code_path() -> PathBuf {
    dir().join(JOIN_CODE_FILE)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportSetting {
    Auto,
    Steam,
    Udp,
}

/// The network keys (top section and [ADVANCED], plus the debug loopback). A change
/// reconnects. Who hosts is not a setting: the co-op items decide (items.rs). An old
/// file's `role` key is ignored like any unknown key.
#[derive(Clone, Debug, PartialEq)]
pub struct NetSettings {
    pub password: String,
    /// Empty = the Steam name, else "Wolf" (resolved by `net`).
    pub player_name: String,
    pub transport: TransportSetting,
    pub udp_host_address: String,
    pub udp_port: u16,
    /// 1 = connect at game start without the items (the old automatic role).
    pub auto_connect: bool,
    /// [DEBUG] host and an echo peer inside this game: the puppet replays you 1 s late.
    pub loopback: bool,
    /// [DEBUG] (0..=5) in-process echo clients of our own host session (net.rs), each
    /// replaying us with its own delay; with `loopback` the number of echo peers.
    pub loopback_peers: u8,
}

impl Default for NetSettings {
    fn default() -> Self {
        Self {
            password: String::new(),
            player_name: String::new(),
            transport: TransportSetting::Auto,
            udp_host_address: String::new(),
            udp_port: DEFAULT_PORT,
            auto_connect: false,
            loopback: false,
            loopback_peers: 0,
        }
    }
}

struct State {
    net: NetSettings,
    mtime: Option<SystemTime>,
    last_check: Instant,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static NET_CHANGED: AtomicBool = AtomicBool::new(false);
static SHOW_STATUS: AtomicBool = AtomicBool::new(true);
static PARTY_BARS: AtomicBool = AtomicBool::new(true);
/// party_hp_position: `PARTY_POS_*`.
static PARTY_POS: AtomicU8 = AtomicU8::new(PARTY_POS_HEAD);
/// party_hp_content: `PARTY_CONTENT_*`.
static PARTY_CONTENT: AtomicU8 = AtomicU8::new(PARTY_CONTENT_NAME_HP);
/// party_hp_scale in percent (50..200).
static PARTY_SCALE: AtomicU32 = AtomicU32::new(100);
pub const PARTY_POS_HEAD: u8 = 0;
pub const PARTY_POS_LEFT: u8 = 1;
pub const PARTY_POS_TOP: u8 = 2;
pub const PARTY_CONTENT_NAME_HP: u8 = 0;
/// "hp": the bar only (the reader treats it as the middle choice; nothing compares to it).
#[allow(dead_code)]
pub const PARTY_CONTENT_HP: u8 = 1;
pub const PARTY_CONTENT_HP_NUMBERS: u8 = 2;
/// overhead_player_display: 0 name + ping, 1 name only, 2 off.
static OVERHEAD: AtomicU8 = AtomicU8::new(0);
/// `observe` as read at start-up.
static OBSERVE_AT_START: AtomicBool = AtomicBool::new(true);

pub fn net() -> NetSettings {
    STATE.lock().ok().and_then(|s| s.as_ref().map(|s| s.net.clone())).unwrap_or_default()
}

/// True once after the network keys changed (or the file first appeared): reconnect.
pub fn take_net_changed() -> bool {
    NET_CHANGED.swap(false, Relaxed)
}

/// Drop the session and build a new one next frame (a co-op item was used, net.rs).
pub fn request_reconnect() {
    NET_CHANGED.store(true, Relaxed);
}

pub fn show_status_overlay() -> bool {
    SHOW_STATUS.load(Relaxed)
}

pub fn party_hp_bars() -> bool {
    PARTY_BARS.load(Relaxed)
}

/// Where the party HP bar goes: `PARTY_POS_HEAD` (under the overhead tag), `_LEFT`, `_TOP`.
pub fn party_hp_position() -> u8 {
    PARTY_POS.load(Relaxed)
}

/// What the party HP bar shows: `PARTY_CONTENT_NAME_HP`, `_HP`, `_HP_NUMBERS`.
pub fn party_hp_content() -> u8 {
    PARTY_CONTENT.load(Relaxed)
}

/// Party HP bar size in percent (50..200).
pub fn party_hp_scale() -> u32 {
    PARTY_SCALE.load(Relaxed)
}

pub fn overhead_display() -> u8 {
    OVERHEAD.load(Relaxed)
}

/// Whether the observation hooks were asked for when the game started.
#[cfg(feature = "testsync")]
pub fn observe_at_start() -> bool {
    OBSERVE_AT_START.load(Relaxed)
}

/// `gpu_guard` and `overlay_hook` ([DEBUG]) as read at start-up (both patch code once).
static GPU_GUARD_AT_START: AtomicBool = AtomicBool::new(true);
static OVERLAY_HOOK_AT_START: AtomicBool = AtomicBool::new(true);
static STALL_WATCHDOG_AT_START: AtomicBool = AtomicBool::new(true);

/// The stall watchdog thread (lib.rs) is started: logs where the main thread sits when no frame ran for 400 ms.
pub fn stall_watchdog_at_start() -> bool {
    STALL_WATCHDOG_AT_START.load(Relaxed)
}

/// The material constant-buffer guard (gpuguard.rs) is installed.
pub fn gpu_guard_at_start() -> bool {
    GPU_GUARD_AT_START.load(Relaxed)
}

/// The hudhook DX11 overlay (ui.rs) is installed. 0 = no Present hook at all (A/B test).
pub fn overlay_hook_at_start() -> bool {
    OVERLAY_HOOK_AT_START.load(Relaxed)
}

/// Key -> value of an ini text. Sections are only for the reader: keys are unique, so the
/// lookup ignores them. Lines starting with `;` or `#` are comments.
struct Ini {
    keys: HashMap<String, String>,
    /// Test only: every key looked up, to catch a key that is misspelled in the default file.
    #[cfg(test)]
    asked: std::cell::RefCell<std::collections::HashSet<String>>,
}

impl Ini {
    fn parse(text: &str) -> Self {
        let mut m = HashMap::new();
        for line in text.lines() {
            let line = line.trim().trim_start_matches('\u{feff}');
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                m.insert(k.trim().to_lowercase(), v.trim().to_owned());
            }
        }
        Self {
            keys: m,
            #[cfg(test)]
            asked: Default::default(),
        }
    }

    fn get(&self, key: &str) -> Option<&String> {
        #[cfg(test)]
        self.asked.borrow_mut().insert(key.to_owned());
        self.keys.get(key)
    }

    fn text(&self, key: &str) -> String {
        self.get(key).cloned().unwrap_or_default()
    }

    fn flag(&self, key: &str, default: bool, warn: &mut Vec<String>) -> bool {
        let Some(v) = self.get(key) else { return default };
        match v.to_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            "" => default,
            _ => {
                warn.push(format!("{key} = {v:?} is not 0/1, using {}", default as u8));
                default
            }
        }
    }

    /// Index of the `options` entry matching the value (case-insensitive). Empty = `default`;
    /// unknown = `default` with a warning.
    fn choice(&self, key: &str, options: &[&str], default: u8, warn: &mut Vec<String>) -> u8 {
        let Some(v) = self.get(key).filter(|v| !v.is_empty()) else { return default };
        match options.iter().position(|o| o.eq_ignore_ascii_case(v)) {
            Some(i) => i as u8,
            None => {
                warn.push(format!("{key} = {v:?} is not {}, using {}", options.join("/"), options[default as usize]));
                default
            }
        }
    }

    /// A decimal number ("1.5") as thousandths, clamped to `min..=max` thousandths.
    fn milli(&self, key: &str, default: u32, min: u32, max: u32, warn: &mut Vec<String>) -> u32 {
        let Some(v) = self.get(key).filter(|v| !v.is_empty()) else { return default };
        match v.replace(',', ".").parse::<f64>() {
            Ok(n) if n.is_finite() => (n * 1000.0).round().clamp(min as f64, max as f64) as u32,
            _ => {
                warn.push(format!("{key} = {v:?} is not a number, using {}", default as f32 / 1000.0));
                default
            }
        }
    }

    fn num(&self, key: &str, default: u32, min: u32, max: u32, warn: &mut Vec<String>) -> u32 {
        let Some(v) = self.get(key).filter(|v| !v.is_empty()) else { return default };
        match v.parse::<i64>() {
            Ok(n) => n.clamp(min as i64, max as i64) as u32,
            Err(_) => {
                warn.push(format!("{key} = {v:?} is not a number, using {default}"));
                default
            }
        }
    }
}

fn parse_net(ini: &Ini, warn: &mut Vec<String>) -> NetSettings {
    let transport = match ini.text("transport").to_lowercase().as_str() {
        "" | "auto" => TransportSetting::Auto,
        "steam" => TransportSetting::Steam,
        "udp" => TransportSetting::Udp,
        other => {
            warn.push(format!("transport = {other:?} is not auto/steam/udp, using auto"));
            TransportSetting::Auto
        }
    };
    NetSettings {
        password: ini.text("password"),
        player_name: ini.text("player_name"),
        transport,
        udp_host_address: ini.text("udp_host_address"),
        udp_port: ini.num("udp_port", DEFAULT_PORT as u32, 1024, 65535, warn) as u16,
        auto_connect: ini.flag("auto_connect", false, warn),
        loopback: ini.flag("loopback", false, warn),
        loopback_peers: ini.num("loopback_peers", 0, 0, crate::peers::MAX_PEERS as u32, warn) as u8,
    }
}

/// Stores every live setting in the statics the rest of the mod reads.
fn apply(ini: &Ini, warn: &mut Vec<String>) {
    // [GAMEPLAY]
    enemies::SHARE_LOOT.store(ini.flag("share_loot", true, warn), Relaxed);
    enemies::SHARE_EXP.store(ini.flag("share_exp", true, warn), Relaxed);
    tuning::BOSS_SPECTATE.store(ini.flag("boss_spectate", true, warn), Relaxed);
    tuning::SPECTATE_CAMERA.store(ini.num("spectate_camera", 1, 0, 2, warn) as u8, Relaxed);
    tuning::SPECTATE_ORBIT.store(ini.num("spectate_orbit", 1, 0, 2, warn) as u8, Relaxed);
    tuning::SPECTATE_ORBIT_SPEED_MILLI.store(ini.milli("spectate_orbit_speed", 1000, 100, 5000, warn), Relaxed);
    tuning::SPECTATE_REPLAY_LOAD.store(ini.flag("spectate_replay_load", true, warn), Relaxed);
    tuning::SPECTATE_LOCK.store(ini.flag("spectate_lock", true, warn), Relaxed);
    tuning::SPECTATE_LOCK_KEY.store(ini.num("spectate_lock_key", 4, 0, 255, warn), Relaxed);
    tuning::SPECTATE_SWITCH_KEY.store(ini.num("spectate_switch_key", 5, 0, 255, warn), Relaxed);
    crate::spectate_input::SPECTATE_MOUSE.store(ini.flag("spectate_mouse", true, warn), Relaxed);
    crate::spectate_input::SPECTATE_MOUSE_SENS_MILLI.store(ini.milli("spectate_mouse_sens", 1000, 100, 10000, warn), Relaxed);
    tuning::SPECTATE_LOCK_HEIGHT_MILLI.store(ini.milli("spectate_lock_height", 1500, 0, 5000, warn), Relaxed);
    tuning::SPECTATE_RECENTER_S.store(ini.num("spectate_recenter_s", 0, 0, 60, warn), Relaxed);
    tuning::BOSS_EXPEL.store(ini.flag("boss_expel", false, warn), Relaxed);
    tuning::DEATH_REFILL.store(ini.flag("death_refill", true, warn), Relaxed);
    tuning::BOSS_REWARD.store(ini.flag("boss_reward", true, warn), Relaxed);
    tuning::AUTO_FOLLOW.store(ini.flag("auto_follow", false, warn), Relaxed);
    tuning::JOIN_REST.store(ini.flag("join_rest", true, warn), Relaxed);
    tuning::DEATH_DEBUFFS.store(ini.flag("death_debuffs", true, warn), Relaxed);
    tuning::INGAME_MENU.store(ini.flag("ingame_menu", false, warn), Relaxed);
    tuning::INGAME_MENU_KEY.store(ini.num("ingame_menu_key", 45, 0, 255, warn), Relaxed);
    tuning::INGAME_MENU_PAD.store(ini.flag("ingame_menu_pad", true, warn), Relaxed);
    tuning::NATIVE_MOD_MENU.store(ini.flag("native_mod_menu", true, warn), Relaxed);
    tuning::NATIVE_MOD_MENU_INWORLD.store(ini.flag("native_mod_menu_inworld", true, warn), Relaxed);
    tuning::DEATH_DEBUFF_ICON.store(ini.num("death_debuff_icon", 0, 0, 1_000_000_000, warn), Relaxed);
    tuning::ENEMY_HEALTH_SCALING.store(ini.num("enemy_health_scaling", 60, 0, 1000, warn), Relaxed);
    tuning::BOSS_HEALTH_SCALING.store(ini.num("boss_health_scaling", 50, 0, 1000, warn), Relaxed);
    tuning::ENEMY_POSTURE_SCALING.store(ini.num("enemy_posture_scaling", 60, 0, 1000, warn), Relaxed);
    tuning::BOSS_POSTURE_SCALING.store(ini.num("boss_posture_scaling", 50, 0, 1000, warn), Relaxed);
    tuning::SCALING_COUNT.store(tuning::parse_scaling_count(&ini.text("scaling_count"), warn), Relaxed);
    tuning::SYNC_ART_EFFECTS.store(ini.flag("sync_art_effects", true, warn), Relaxed);
    tuning::MERGE_QUESTS.store(ini.num("merge_quests", 1, 0, 2, warn) as u8, Relaxed);
    crate::npcintro::SHARED.store(ini.flag("shared_npc_intros", false, warn), Relaxed);
    crate::talklock::HOST_ONLY.store(ini.flag("quest_choices_host_only", false, warn), Relaxed);
    crate::resurrect::ENABLED.store(ini.flag("share_resurrect_unlock", true, warn), Relaxed);
    crate::ownworld::OWN_WORLD_MODE.store(crate::ownworld::parse_mode(&ini.text("own_world_mode"), warn), Relaxed);
    tuning::SYNC_POSTURE.store(ini.flag("sync_posture", true, warn), Relaxed);
    crate::keyitems::ENABLED.store(ini.flag("share_key_items", true, warn), Relaxed);
    crate::bossgate::ENABLED.store(ini.flag("boss_start_gate", true, warn), Relaxed);
    crate::dialog::CONFIRM_ITEMS.store(ini.flag("confirm_items", true, warn), Relaxed);
    tuning::DIALOG_DEFAULT_YES.store(ini.flag("dialog_default_yes", true, warn), Relaxed);
    crate::leave::RELOAD_ON_LEAVE.store(ini.flag("reload_on_leave", true, warn), Relaxed);
    crate::quitblock::ENABLED.store(ini.flag("block_quit_in_coop", true, warn), Relaxed);
    crate::ngblock::ENABLED.store(ini.flag("block_ng_in_coop", true, warn), Relaxed);
    crate::doctor::ALLOW_CROSS_CYCLE.store(ini.flag("allow_cross_cycle", false, warn), Relaxed);
    crate::nopause::ENABLED.store(ini.flag("no_pause_in_coop", true, warn), Relaxed);
    crate::livesync::ENABLED.store(ini.flag("native_lives_sync", true, warn), Relaxed);
    crate::puppetguard::ENABLED.store(ini.flag("puppet_type5_guard", true, warn), Relaxed);
    crate::bossrules::CLEAR_STALE_NO_REVIVE.store(ini.flag("clear_stale_no_revive", true, warn), Relaxed);
    crate::persist::BOSS_PERSIST.store(ini.flag("boss_persist_on_death", true, warn), Relaxed);
    enemies::STEALTH_WINDOW_M.store(ini.num("stealth_window_m", 15, 0, 40, warn), Relaxed);

    // [DISPLAY]. vsync = 0 means "turn the game's vsync off" (fps.rs VSYNC_OFF).
    fps::UNLOCK.store(ini.flag("fps_unlock", true, warn), Relaxed);
    fps::VSYNC_OFF.store(!ini.flag("vsync", false, warn), Relaxed);
    OVERHEAD.store(ini.num("overhead_player_display", 0, 0, 2, warn) as u8, Relaxed);
    SHOW_STATUS.store(ini.flag("show_status_overlay", true, warn), Relaxed);
    PARTY_BARS.store(ini.flag("party_hp_bars", true, warn), Relaxed);
    PARTY_POS.store(ini.choice("party_hp_position", &["head", "left", "top"], PARTY_POS_HEAD, warn), Relaxed);
    PARTY_CONTENT.store(ini.choice("party_hp_content", &["name_hp", "hp", "hp_numbers"], PARTY_CONTENT_NAME_HP, warn), Relaxed);
    PARTY_SCALE.store(ini.num("party_hp_scale", 100, 50, 200, warn), Relaxed);
    tuning::CUSTOM_ICONS.store(ini.flag("custom_icons", true, warn), Relaxed);
    crate::dialog::NATIVE_NOTICES.store(ini.flag("native_notices", false, warn), Relaxed);

    // [ADVANCED], live (no reconnect): the native session (nsession.rs).
    tuning::NATIVE_SESSION.store(ini.flag("native_session", true, warn), Relaxed);
    let max_stage = crate::nsession::MAX_STAGE as u32;
    tuning::NATIVE_SESSION_STAGE.store(ini.num("native_session_stage", 3, 1, max_stage, warn) as u8, Relaxed);
    // 3-6 players (PLAN-MULTIPLAYER): the host's world size.
    let max_players = crate::peers::MAX_PLAYERS as u32;
    tuning::MAX_PLAYERS.store(ini.num("max_players", 6, 2, max_players, warn) as u8, Relaxed);
    let native_max = ini.num("native_session_max_players", max_players, 2, max_players, warn);
    crate::nsession::MAX_NATIVE_PLAYERS.store(native_max as u8, Relaxed);

    // [DEBUG]
    tuning::NATIVE_REMOTE.store(ini.flag("native_remote", true, warn), Relaxed);
    tuning::ENEMY_MODE.store(ini.num("enemy_mode", tuning::ENEMY_NETAI as u32, 0, tuning::ENEMY_NETAI as u32, warn) as u8, Relaxed);
    tuning::DEBUG_HOTKEYS.store(ini.flag("debug_hotkeys", false, warn), Relaxed);
    tuning::OPTIONS_PROBE.store(ini.flag("options_probe", true, warn), Relaxed);
    {
        let i = ini.choice("f11_pad_button", &["back", "start", "l3", "r3", "none"], 0, warn);
        tuning::F11_PAD_MASK.store([0x0020u16, 0x0010, 0x0040, 0x0080, 0][i as usize], Relaxed);
    }
    tuning::GHOST_DELAY_MS.store(ini.num("ghost_delay_ms", 80, 0, 250, warn), Relaxed);
    tuning::GHOST_ANGLE.store(ini.flag("ghost_angle", true, warn), Relaxed);
    tuning::GHOST_HKB.store(ini.flag("ghost_hkb", false, warn), Relaxed);
    tuning::GHOST_WEVENT.store(ini.flag("ghost_wevent", false, warn), Relaxed);
    tuning::GHOST_SOLID.store(ini.num("ghost_solid", 1, 0, 2, warn) as u8, Relaxed);
    tuning::GHOST_MOTION.store(ini.flag("ghost_motion", true, warn), Relaxed);
    tuning::JOIN_RELOAD.store(ini.flag("join_reload", true, warn), Relaxed);
    tuning::ENEMY_NO_MOVE.store(ini.flag("enemy_no_move", false, warn), Relaxed);
    tuning::WORLD_ON_JOIN.store(ini.flag("world_on_join", true, warn), Relaxed);
    tuning::ENEMY_DELAY_MS.store(ini.num("enemy_delay_ms", 120, 0, 250, warn), Relaxed);
    tuning::ENEMY_SMOOTH_PCT.store(ini.num("enemy_smooth_pct", 20, 1, 100, warn), Relaxed);
    tuning::NATIVE_SOLID.store(ini.flag("native_solid", true, warn), Relaxed);
    tuning::NATIVE_DRESS.store(ini.flag("native_dress", true, warn), Relaxed);
    tuning::NATIVE_SPAWN_DELAY_S.store(ini.num("native_spawn_delay_s", 10, 2, 120, warn), Relaxed);
    tuning::NATIVE_DRESS_DELAY_MS.store(ini.num("native_dress_delay_ms", 3000, 0, 60_000, warn), Relaxed);
    tuning::N0_CLONES.store(ini.num("n0_clones", 0, 0, 5, warn), Relaxed);
    tuning::RELAY_SNAPSHOT_HZ.store(ini.num("relay_snapshot_hz", 30, 10, 120, warn), Relaxed);
    tuning::SNAPSHOT_HZ_MULTI.store(ini.num("snapshot_hz_multi", 60, 20, 120, warn), Relaxed);
    tuning::DEATH_RESTORE_DELAY_S.store(ini.num("death_restore_delay_s", 8, 2, 60, warn), Relaxed);
    tuning::RESTORE_LOST_ITEMS.store(ini.flag("restore_lost_items", true, warn), Relaxed);
    items::ENABLED.store(ini.flag("connection_items", true, warn), Relaxed);
    crate::audit::ENABLED.store(ini.flag("audit", true, warn), Relaxed);
    crate::audit::EVERY_S.store(ini.num("audit_every_s", 180, 30, 3600, warn), Relaxed);
    // v7.8 bisect switches for the v7.7 behaviours (live 2026-10-24: input lock at join,
    // fps drops, posture not reset after a deathblow). 0 = the v7.6 behaviour.
    // v7.9: forced distribution back on by default (deflect timing was off without it).
    crate::nsession::FORCE_DISTRIBUTION.store(ini.flag("ns_force_distribution", true, warn), Relaxed);
    crate::nsession::THROW_CLAIMS_ON.store(ini.flag("ns_throw_claims", false, warn), Relaxed);
    crate::nsession::THROW_COLLIDE_DROP.store(ini.flag("ns_throw_collide_drop", false, warn), Relaxed);
    crate::puppet::SCALE_ALL_LOADED.store(ini.flag("scale_all_loaded", false, warn), Relaxed);
    crate::posture::REST_GUARD.store(ini.flag("posture_rest_guard", true, warn), Relaxed);
    crate::keyitems::RECONCILE.store(ini.flag("reward_reconcile", true, warn), Relaxed);
    crate::statelog::ENABLED.store(ini.flag("state_log", true, warn), Relaxed);
    crate::hook::freeze::SKIP_DRIVER_THREADS.store(ini.flag("freeze_skip_driver_threads", true, warn), Relaxed);
    // Log categories switched off (log.rs). Missing key = the default list; an empty value = none.
    log::set_muted(log::parse_categories(ini.get("log_off").map_or(DEFAULT_LOG_OFF, String::as_str)));
    // Read once at start: the observation hooks stay once installed.
    if STATE.lock().is_ok_and(|s| s.is_none()) {
        OBSERVE_AT_START.store(ini.flag("observe", true, warn), Relaxed);
        GPU_GUARD_AT_START.store(ini.flag("gpu_guard", true, warn), Relaxed);
        OVERLAY_HOOK_AT_START.store(ini.flag("overlay_hook", true, warn), Relaxed);
        STALL_WATCHDOG_AT_START.store(ini.flag("stall_watchdog", true, warn), Relaxed);
    }
}

/// Log categories off unless the settings say otherwise: the netcode call counters (observe) and
/// the behaviour-graph event counters (hkb), not read in an analysis since v7, and the per-frame
/// "native: remote moved ... off our pin" (15 % of a log; the 10 s "native: pin" line counts them).
const DEFAULT_LOG_OFF: &str = "observe, hkb, native: remote moved";

const TEMPLATE: &str = "\
; Sekiro seamless co-op settings. Lines starting with ; are comments.
; The game re-reads this file about every 2 seconds, so most changes apply at once.
;
; How to play together: both players write the same password below. After the
; tutorial, rest at the Dilapidated Temple idol: you get three co-op items (Goods).
;   Kindred Bell   = open your world (host).
;   Kindred Charm  = find your friend's world and join it.
;   Severed Thread = leave co-op / close your world.

; Both players must use the same password. Co-op stays off while it is empty.
; It is never sent anywhere as text, only as a one-way hash.
password = @PASSWORD@
; Name shown above your head. Empty = your Steam name.
player_name = @NAME@

[GAMEPLAY]
; Each player gets their own drops and experience from enemies the friend killed.
share_loot = 1
share_exp = 1
; Boss fights: after you die, wait on the death screen until the boss falls or the friend dies.
boss_spectate = 1
; While you wait (boss_spectate = 1): 1 = no death screen, the camera follows a living player;
; 2 = the death screen stays, the camera follows them behind it; 0 = death screen only.
spectate_camera = 1
; Spectator camera steering: 1 = the game's own camera input (stick / mouse) orbits the view,
; 2 = gamepad right stick, 0 = fixed camera behind the watched player (the old behaviour).
spectate_orbit = 1
; Stick orbit speed multiplier (1.0 = 150 degrees per second).
spectate_orbit_speed = 1.0
; 1 = lock-on while spectating (toggle with the key below or the right stick click).
spectate_lock = 1
; Lock-on toggle key, Windows virtual-key code in decimal (4 = middle mouse, 0 = none).
spectate_lock_key = 4
; 3+ players: key that moves the spectator camera to the next living player (virtual-key
; code in decimal, 5 = mouse side button X1, 0 = none). Gamepad: D-pad right / left.
spectate_switch_key = 5
; 1 = while spectating, mouse movement and the arrow keys steer the camera when the game's own
; camera input is frozen (a gamepad right stick wins when both are used). 0 = off.
spectate_mouse = 1
; Mouse look sensitivity multiplier for the spectator camera (1.0 = 0.15 degrees per pixel).
spectate_mouse_sens = 1.0
; Aim point of the lock-on above the target's feet, in metres.
spectate_lock_height = 1.5
; Seconds without steering before the camera swings behind the watched player again (0 = never).
spectate_recenter_s = 0
; 1 (default) = a spectator released on a wipe replays the game's own held death-warp request (keeps
; emblems, storage, Resurrect nodes). 0 = the old null-argument reload (known to wipe those items).
spectate_replay_load = 1
; Boss fights: 1 = when the friend rests at an idol, you are sent to your idol too. 0 (default) = you stay.
boss_expel = 0
; 1 (default) = after a death reload you wake with full HP, gourd, Resurrection and restocked items, like a rest, but enemies do not respawn. 0 = off.
death_refill = 1
; Boss fights: the friend also receives the boss's item rewards.
boss_reward = 1
; 1 = follow the friend's idol warps automatically. 0 = free travel.
auto_follow = 0
; 1 = when you join, rest at the arrival idol so enemies you had killed are alive again.
join_rest = 1
; 1 = every death in co-op lowers your max HP and posture by 10 % (max 5 stacks, 50 %).
;     Resting at an idol yourself clears it. 0 = off.
death_debuffs = 1
; 1 = while the game's System menu (the list with Quit Game) is open, a 'Mod settings' panel
;     appears at the right edge of the screen. The host edits the enemy / boss scaling and
;     the death debuff there (applied at once, saved to this file, sent to the friend); a
;     client sees the host's values read-only. 0 = no panel at all.
ingame_menu = 0
; Virtual-key code (decimal, 0..255) that collapses / expands the panel. 45 = Insert.
ingame_menu_key = 45
; 1 = the panel can also be driven with the controller (host only): right stick up/down = row,
;     right stick left/right = value (hold LB or RB for steps of 25), right trigger = Apply,
;     R3 = flip the death debuff. The D-pad, A and B are never used. If the game's System menu
;     also reacts to these inputs, set 0 and use the mouse. 0 = no controller control.
ingame_menu_pad = 1
; 1 = replace the game's 'Network Settings' page in the Options menu with a native 'Co-op Settings'
;     page (EXPERIMENTAL; needs the matching page code, off by default). 0 = the page stays as it is.
native_mod_menu = 1
; 1 = the native page is also used when Options is opened inside a world (v8.43: the crash was the 32-bit
;     truncated parent pointer, fixed). 0 = inside a world the game shows its original page.
native_mod_menu_inworld = 1
; 1 = the death debuff is a status effect with an icon that also lowers attack power by 5 %
;     per stack. 0 = max HP / posture only, no icon. A larger number = use that icon id.
death_debuff_icon = 0
; Percent added to enemy and boss health / posture for the second player
; (0 = no scaling). The host's values are used on both machines. Each further player
; adds less: health 85 % of the step before, posture 60 % (six players with the
; defaults: boss health 6.6x, boss posture 2.8x).
enemy_health_scaling = 60
boss_health_scaling = 50
enemy_posture_scaling = 60
boss_posture_scaling = 50
; With 3 or more players: session = every connected player counts for the scaling above,
; block = only the players in the host's map area. The host's value is used.
scaling_count = session
; 1 = the friend's combat art and prosthetic effects (wind, spiral cloud, shuriken...)
;     appear on their character here. Copies deal no damage: each player's hits count
;     on their own machine only. 0 = off.
sync_art_effects = 1
; Joiner: NPC quest progress made in the host's world is kept in your own world when
; co-op ends (each NPC as a whole). 0 = off (NPC quests back to how they were),
; 2 = coarser (whole map areas of NPC state) if a quest breaks with 1.
merge_quests = 1
; 0 = each player hears an NPC's introduction: when the friend talked to an NPC you
;     never met, its answers reach you after you have talked to it yourself (then the
;     friend's decisions count for both). 1 = shared at once (you skip the introduction).
shared_npc_intros = 0
; 1 = in a co-op world you joined, only the host can talk to quest NPCs (the ones whose
;     conversations change their story): you get a notice instead, and the host ignores
;     what you would have decided. 0 = everybody talks (while one player talks to an NPC,
;     the others cannot start a conversation with that NPC either way).
quest_choices_host_only = 0
; 1 = after a Resurrect the next one unlocks for everybody when ANY player lands a
;     deathblow / kill (Sekiro unlocks it only for the one who did it). Nodes are not
;     given: a player with no Resurrection node left still cannot resurrect. 0 = own only.
share_resurrect_unlock = 1
; 1 = after the mod's own death reload (a spectator released on a wipe) the game would leave
;     you with 0 Spirit Emblems and 0 sen; give back exactly what you carried just before
;     (sen halved, like a normal death). 0 = off (the snapshot is still logged).
restore_lost_items = 1
; Joiner, when co-op ends: auto = if your own world had nothing the host's lacked
;     (you play the whole game together), keep the shared world as yours (mirror),
;     else your own world back + the session's progress (merge). mirror / merge = always.
;     Your items, shop purchases, skills and death state are always your own.
own_world_mode = auto
; 1 = the friend's hits also fill the posture bar of the host's enemies and bosses, and
;     the friend's deathblows use up a boss's life on the host too. 0 = HP only.
sync_posture = 1
; 1 = items an NPC or an event gives you (Healing Gourd, Mortal Blade, esoteric texts,
;     prosthetic tools, Prayer Beads...) are given to the friend too, and on connecting
;     both of you are told which key items the other lacks. World pickups and shops stay
;     your own. 0 = off.
share_key_items = 1
; 1 = a boss fight the friend starts far from you (other area or over 60 m) does not
;     start in your world until you come near it; the kill still counts for you. 0 = off.
boss_start_gate = 1
; 1 = the co-op items ask first in the game's own yes/no box (Kindred Bell, Kindred Charm,
;     Severed Thread; the Bell while connected asks to resync). 0 = they act at once.
confirm_items = 1
; 1 = in those yes/no boxes the Yes button is highlighted when the box opens (pressing
;     the confirm key answers Yes). 0 = the game's own default, No highlighted.
dialog_default_yes = 1
; 1 = leaving a companion's world (Severed Thread, the host closes their world, link lost)
;     reloads your world at your last idol, through a loading screen. Never during a boss
;     fight or a menu: it waits. 0 = you stay where you are.
reload_on_leave = 1
; 1 = while a companion is connected, Quit Game / return to title from the pause menu
;     is cancelled with a notice: use the Severed Thread first (default). 0 = off: Quit
;     Game works while connected.
block_quit_in_coop = 1
; 1 = while a companion is connected (or the ending started while connected), the New Game+
;     answer after the credits is refused with a notice and the game takes its no path;
;     leave co-op and use the Dilapidated Temple idol instead. 0 = NG+ may start.
block_ng_in_coop = 1
; 0 = if you and your companion are on different NG cycles (NG, NG+1...), co-op is refused
;     with a notice and the client leaves (enemy HP would differ). 1 = allowed on both PCs
;     (set it on both), with a warning only.
allow_cross_cycle = 0
; 1 = while a companion is connected, menus, the inventory, item pickup messages and the
;     co-op yes/no boxes do not pause the game (as in FromSoft's online play): both worlds
;     keep running. Solo the game pauses as usual. 0 = menus pause as in single player.
no_pause_in_coop = 1
; 1 = with the native session, a boss's or miniboss's lost life (deathblow) and an enemy's
;     death on either machine are applied on the other one too (within about half a
;     second), so both see the same life orbs and the same dead enemies. 0 = off.
native_lives_sync = 1
; 1 = a Puppeteer Ninjutsu puppet (an enemy you raised to fight for you) is not killed by
;     the host's copy of that enemy, which stays dead there (native session). 0 = off.
puppet_type5_guard = 1
; 1 = after a death without the game's death screen (spectating), the game's leftover
;     no-Resurrect mark is cleared once you are alive again, so your next death can offer
;     Resurrect as usual. 0 = off (the next death after spectating offers no Resurrect).
clear_stale_no_revive = 1
; 1 = a boss or miniboss keeps its lost health and lives when one player dies and reloads
;     (only a rest or both players dying resets it). 0 = as in the base game (and other
;     seamless co-op mods): a death reload brings the boss back at full health.
boss_persist_on_death = 1
; Stealth and plunging deathblows: an enemy you approach from behind or from above
; within this many metres runs on your own machine's AI (its awareness is yours, as in
; single player). 0 = the old 5 m check behind it only. 0..40.
stealth_window_m = 15

[DISPLAY]
; 1 = frame rate unlocked to 120 fps.
fps_unlock = 1
; 0 = the game's vsync is turned off (needed for 120 fps on a 60 Hz screen, tears a little).
vsync = 0
; Above the friend's head: 0 = name and ping, 1 = name only, 2 = nothing.
overhead_player_display = 0
; 1 = the other players' health bars (master switch for the three settings below).
party_hp_bars = 1
; Where the bars are drawn: head = under each player's name tag above their head (follows
; them; at the screen edge it moves with the tag), left = a list on the left of the screen,
; top = a list at the top left under the status line (one row per player in both).
party_hp_position = head
; What it shows: name_hp = name and bar (the name only in left/top; in head mode the tag
; already has it), hp = the bar only, hp_numbers = the bar and hp / max written next to it.
party_hp_content = name_hp
; Bar size in percent, 50..200.
party_hp_scale = 100
; 1 = the co-op status line in the top left corner.
show_status_overlay = 1
; 1 = the co-op items have their own icons (drawn into the menu textures while the game
;     starts, nothing on disk is changed). 0 = vanilla pictures. Takes effect after a restart.
custom_icons = 1
; 1 = a few other co-op messages as the game's own on-screen message. 0 = top left only
;     (default; the game's message once left the menu stuck). Join / leave messages (a
;     companion entered your world, the host closed their world, back in your own world)
;     are always in the top left, whatever this says.
native_notices = 0

[ADVANCED]
; Changing anything here (or the password / name) restarts the connection.
; auto = Steam; if Steam does not work within 15 seconds, direct UDP instead.
; steam = Steam only. udp = direct UDP only (the host's port must be reachable).
transport = @TRANSPORT@
; Joining over UDP: the host's IP address, e.g. 203.0.113.7 or 203.0.113.7:28100.
; A UDP host finds its address in udp_join_code.txt next to this file.
udp_host_address = @HOST@
; UDP port the host listens on (opened in the router with UPnP).
udp_port = 28100
; 1 = connect at game start without the items (find a friend with the same
;     password, or open your world). 0 = the items decide.
auto_connect = 0
; 1 = enemies are synced by the game's own online code (like the official online
;     mode), carried over this mod's connection; if it fails, this mod's own enemy
;     sync takes over from the next loading screen. 0 = this mod's own sync only.
;     Both players must use the same value. Set it before starting the game.
native_session = 1
; With native_session = 1: 1 = only count the game's packets (changes nothing),
;     2 = enemies and hits on them, 3 = + the other small player packets,
;     4 = + doors / levers (experimental). Same value on both machines.
native_session_stage = 3
; Host: how many players your world takes, you included (2..6). 3 or more is
;     not tested live with 3+ PCs yet. A changed value also resizes your Steam lobby.
max_players = 6
; Host: the native session (native_session = 1) runs while your world has at most
;     this many players (2..6); with more, this mod's own enemy sync for everyone.
;     2 = the native session only in 2-player co-op.
native_session_max_players = 6

[DEBUG]
; Leave these alone unless you are testing. The defaults are the tested setup.
; 0 = no co-op items (then use auto_connect = 1).
connection_items = 1
; 1 = F8 wakes you at the idol while waiting on the boss death screen, F9 goes to the friend,
; F11 writes a MARK line to the log, F3 resyncs the world (both players rest).
debug_hotkeys = 0
; 1 = log-only probe of the same Options menu (menu entries, page kinds, text ids) for a later
; native Co-op Settings page. Changes nothing in the game. 0 = install nothing. Start-up only.
options_probe = 1
; A gamepad button that also writes the F11 MARK line / state dump: back, start, l3, r3 or
; none. Under Steam Input the PS4/PS5 Share button usually arrives as Back. If Share does
; something else in your Steam controller layout, unbind that there or map Share to the
; keyboard key F11 instead.
f11_pad_button = back
; The friend is shown as a real player (native remote). 0 = replay ghost.
native_remote = 1
; Enemies: 0 off, 1 HP and deaths, 2 + positions, 3 + host animations, 4 host AI (NetAI).
enemy_mode = 4
; Log what the game's own netcode does (read at game start only).
observe = 1
; Log lines left out of log.txt, comma list. A word is a whole category (the first word of a
; line: gpuguard, native, nsession, t, ownfill, stealth, observe, hkb, ...), text with a : or a
; space is the start of one line type (native: remote moved). Empty = log everything. MARK and
; CRASH lines always stay. Every clock line (10 min) counts what was left out.
log_off = observe, hkb, native: remote moved
; Host and an echo peer inside this game: the friend is you, 1 second late. Test alone.
loopback = 0
; Test of 3-6 players on one PC: while you host (or with loopback = 1), this many extra
; players (0..5) join from inside your game, each replaying you a bit later (1 s, 1.25 s, ...).
; They need room: max_players at least 1 + friends + loopback_peers.
loopback_peers = 0
ghost_delay_ms = 80
ghost_angle = 1
ghost_hkb = 0
ghost_wevent = 0
ghost_solid = 1
ghost_motion = 1
join_reload = 1
enemy_no_move = 0
world_on_join = 1
enemy_delay_ms = 120
enemy_smooth_pct = 20
native_solid = 1
native_dress = 1
; Driver crash (nvwgf2umx.dll) mitigations, see gpuguard.rs. Read at game start:
; 1 = the game's material constants are uploaded zero-padded (no read past their end).
gpu_guard = 1
; 0 = the in-game overlay (status line, names, menu) is not installed at all.
overlay_hook = 1
; Read at game start. 1 = a diagnostic thread logs where the game's main thread sits when no
; frame ran for 400 ms (log lines starting with stall:). 0 = off.
stall_watchdog = 1
; Seconds after a load before the friend's native character is created (2..120).
native_spawn_delay_s = 10
; Milliseconds after its creation before it gets your equipment (0 = at once).
native_dress_delay_ms = 3000
; One-machine test of a 6-player world: this many local copies of you (0..5) appear,
; one every 3 s, after every load (native_spawn_delay_s). 0 = off. The friend stays a ghost meanwhile.
n0_clones = 0
; With 3 or more players: how often per second the host forwards one player's position to
; the others, and how often you send your own (with one friend: every frame).
relay_snapshot_hz = 30
snapshot_hz_multi = 60
; Host, after its own death: seconds after the reload before the friend's dead enemies die here.
death_restore_delay_s = 8
; Test builds: the auditor compares both players' items, skills, item lots and shared flags,
; boss HP and the reward hand-over, and writes what differs to audit.txt (it never gives
; anything). 0 = off. audit_every_s = seconds between the regular comparisons (30..3600).
audit = 1
audit_every_s = 180
; Test build: every 0.5 s the player's input state (SpEffects, action-ban bits, menu
; states) goes to the log when it changes; F11 writes the full state once. 0 = off.
state_log = 1
; 1 = code patches leave GPU driver threads running (they never run game code; waiting
;     for one froze the game for up to 5 s). 0 = every thread is paused, as before v8.61.
freeze_skip_driver_threads = 1
; v7.7 behaviours, switchable for testing (0 = as in v7.6). Same value on both machines.
; Claims / contests force the game's ownership hand-out at once (max once per second).
ns_force_distribution = 1
; Our deathblow claims its enemy for 4 s (it runs on our AI).
ns_throw_claims = 0
; The friend's deathblow on an enemy ours is deathblowing is not applied here.
ns_throw_collide_drop = 0
; 2-player scaling over every loaded enemy each second (0 = the 45 s scan list).
scale_all_loaded = 0
; No posture lives mirroring for 13 s after a rest.
posture_rest_guard = 1
; Item lot ledger and queued item grants on connect / after a rest.
reward_reconcile = 1
";

/// Values carried over from the old coop.ini.
#[derive(Default)]
struct Legacy {
    password: String,
    name: String,
    host: String,
}

fn read_legacy() -> Legacy {
    let Ok(text) = fs::read_to_string(legacy_dir().join("coop.ini")) else { return Legacy::default() };
    let ini = Ini::parse(&text);
    let password = ini.text("password");
    Legacy {
        // The old template's placeholders are not settings.
        password: if password == "change-me" { String::new() } else { password },
        name: ini.text("name"),
        host: ini.text("host"),
    }
}

/// One on/off key read straight from the file, before `init` (no default file written, no
/// warnings). For patches installed at process start (icons.rs `custom_icons`).
pub fn early_flag(key: &str, default: bool) -> bool {
    let Ok(text) = fs::read_to_string(path()) else { return default };
    Ini::parse(&text).flag(key, default, &mut Vec::new())
}

/// Reads the file (writing the commented default first if it is missing).
fn load_text() -> Option<String> {
    let path = path();
    match fs::read_to_string(&path) {
        Ok(t) => Some(t),
        Err(_) => {
            let legacy = read_legacy();
            let migrated = !legacy.password.is_empty() || !legacy.name.is_empty() || !legacy.host.is_empty();
            // The legacy host address means the old UDP/Tailscale setup: keep using UDP.
            let transport = if legacy.host.is_empty() { "auto" } else { "udp" };
            let name = if legacy.name == "Wolf" { "" } else { legacy.name.as_str() };
            let text = TEMPLATE
                .replace("@PASSWORD@", &legacy.password)
                .replace("@NAME@", name)
                .replace("@TRANSPORT@", transport)
                .replace("@HOST@", &legacy.host);
            match fs::write(&path, &text) {
                Ok(()) => {
                    log::line(&format!(
                        "settings: wrote default {}{}",
                        path.display(),
                        if migrated { " (password / name / host address carried over from the old coop.ini)" } else { "" }
                    ));
                    Some(text)
                }
                Err(e) => {
                    log::line(&format!("settings: could not write {}: {e}, using built-in defaults", path.display()));
                    None
                }
            }
        }
    }
}

fn describe(n: &NetSettings) -> String {
    format!(
        "transport {:?}, udp {}:{}, name {:?}, password {}, auto_connect {}, loopback {}, loopback_peers {}",
        n.transport,
        if n.udp_host_address.is_empty() { "-" } else { n.udp_host_address.as_str() },
        n.udp_port,
        n.player_name,
        if n.password.is_empty() { "NOT SET" } else { "set" },
        n.auto_connect,
        n.loopback,
        n.loopback_peers
    )
}

/// Loads the settings and applies them. Call once at start-up, before the patches that
/// read them (fps, observe) are installed.
pub fn init() {
    let path = path();
    let text = load_text().unwrap_or_default();
    let ini = Ini::parse(&text);
    let mut warn = Vec::new();
    let net = parse_net(&ini, &mut warn);
    apply(&ini, &mut warn);
    for w in &warn {
        log::line(&format!("settings: {w}"));
    }
    log::line(&format!("settings: loaded {} ({})", path.display(), describe(&net)));
    log::line(&format!("tuning: {}", tuning::describe()));
    let mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
    if let Ok(mut s) = STATE.lock() {
        *s = Some(State { net, mtime, last_check: Instant::now() });
    }
}

/// Call every frame: re-reads the file when it changed (checked every 2 s).
pub fn tick() {
    let Ok(mut guard) = STATE.lock() else { return };
    let Some(state) = guard.as_mut() else { return };
    if state.last_check.elapsed() < CHECK_EVERY {
        return;
    }
    state.last_check = Instant::now();
    let path = path();
    let Some(mtime) = fs::metadata(&path).and_then(|m| m.modified()).ok() else { return };
    if state.mtime == Some(mtime) {
        return;
    }
    // An editor that truncates before it writes leaves an empty file for a moment: wait.
    let Some(text) = fs::read_to_string(&path).ok().filter(|t| !t.trim().is_empty()) else { return };
    state.mtime = Some(mtime);
    let ini = Ini::parse(&text);
    let mut warn = Vec::new();
    let net = parse_net(&ini, &mut warn);
    // `apply` looks at STATE for the start-up-only `observe`: it is set by now.
    drop(guard);
    apply(&ini, &mut warn);
    for w in &warn {
        log::line(&format!("settings: {w}"));
    }
    log::line("settings: reloaded");
    log::line(&format!("tuning: {}", tuning::describe()));
    let Ok(mut guard) = STATE.lock() else { return };
    let Some(state) = guard.as_mut() else { return };
    if state.net != net {
        log::line(&format!("settings: network keys changed, reconnecting ({})", describe(&net)));
        state.net = net;
        NET_CHANGED.store(true, Relaxed);
    }
}

/// Replaces the VALUE of the `key = value` lines named in `upd` (case-insensitive key) and
/// changes nothing else: the key's spelling, indentation, a trailing `; comment`, a UTF-8
/// BOM and the CRLF / LF line endings stay. Commented-out lines are never touched. A key
/// that is not in the text is appended after the last line of the [GAMEPLAY] section (or at
/// the end of the file when there is no such section). Running it again with the same
/// values changes nothing.
pub fn rewrite_ini(text: &str, upd: &[(&str, String)]) -> String {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_owned).collect();
    let mut found = vec![false; upd.len()];
    let mut gameplay: Option<usize> = None;
    let mut gameplay_end: Option<usize> = None;
    for i in 0..lines.len() {
        let raw = lines[i].as_str();
        let body = raw.trim_end_matches(['\r', '\n']);
        let term = &raw[body.len()..];
        let bom_len = if body.starts_with('\u{feff}') { '\u{feff}'.len_utf8() } else { 0 };
        let content = &body[bom_len..];
        let t = content.trim();
        if t.starts_with('[') {
            if t.to_ascii_lowercase().starts_with("[gameplay]") {
                gameplay = Some(i);
            } else if gameplay.is_some() && gameplay_end.is_none() {
                gameplay_end = Some(i);
            }
            continue;
        }
        if t.is_empty() || t.starts_with(';') || t.starts_with('#') {
            continue;
        }
        let Some(eq) = content.find('=') else { continue };
        let key = content[..eq].trim();
        let Some(n) = upd.iter().position(|(k, _)| k.eq_ignore_ascii_case(key)) else { continue };
        found[n] = true;
        let after = &content[eq + 1..];
        let lead = after.len() - after.trim_start().len();
        let val_part = &after[lead..];
        let comment = if val_part.starts_with(';') {
            Some(0)
        } else {
            [val_part.find(" ;"), val_part.find("\t;")].into_iter().flatten().min()
        };
        let vlen = val_part[..comment.unwrap_or(val_part.len())].trim_end().len();
        let new = format!("{}{}{}{}", &body[..bom_len + eq + 1 + lead], upd[n].1, &val_part[vlen..], term);
        lines[i] = new;
    }
    let missing: Vec<String> =
        upd.iter().zip(&found).filter(|(_, f)| !**f).map(|((k, v), _)| format!("{k} = {v}{eol}")).collect();
    if !missing.is_empty() {
        let pos = match gameplay {
            Some(s) => {
                let end = gameplay_end.unwrap_or(lines.len());
                (s..end).rev().find(|&j| !lines[j].trim().is_empty()).map_or(end, |j| j + 1)
            }
            None => lines.len(),
        };
        if pos > 0 && !lines[pos - 1].ends_with('\n') {
            lines[pos - 1].push_str(eol);
        }
        lines.splice(pos..pos, missing);
    }
    lines.concat()
}

/// Writes the keys into the settings file: reads the file fresh, rewrites it, writes
/// `<file>.tmp` and renames it over the original. The modification time is left alone: the
/// next `tick` does one normal reload with identical values (a reload never writes).
pub fn write_keys(upd: &[(&str, String)]) -> Result<(), String> {
    let path = path();
    let text = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let new = rewrite_ini(&text, upd);
    if new == text {
        return Ok(());
    }
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    fs::write(&tmp, &new).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("rename over {}: {e}", path.display())
    })
}

/// The settings file as text (for the native Co-op Settings page, optmenu.rs). `None` if unreadable.
pub fn ini_text() -> Option<String> {
    fs::read_to_string(path()).ok()
}

/// The commented default file with the placeholders blanked: the built-in default of every key.
pub fn default_ini_text() -> String {
    TEMPLATE.replace("@PASSWORD@", "").replace("@NAME@", "").replace("@TRANSPORT@", "auto").replace("@HOST@", "")
}

#[cfg(test)]
mod rewrite_tests {
    use super::*;

    fn u(k: &'static str, v: &str) -> (&'static str, String) {
        (k, v.to_owned())
    }

    #[test]
    fn replaces_only_the_value_keeps_comment_and_spelling() {
        let t = "[GAMEPLAY]\n; boss_health_scaling = 1\n  Boss_Health_Scaling =  50  ; keep me\nother = 1\n";
        let r = rewrite_ini(t, &[u("boss_health_scaling", "120")]);
        assert_eq!(r, "[GAMEPLAY]\n; boss_health_scaling = 1\n  Boss_Health_Scaling =  120  ; keep me\nother = 1\n");
    }

    #[test]
    fn keeps_crlf_and_bom() {
        let t = "\u{feff}[GAMEPLAY]\r\ndeath_debuffs = 1\r\nx = 2\r\n";
        let r = rewrite_ini(t, &[u("death_debuffs", "0")]);
        assert_eq!(r, "\u{feff}[GAMEPLAY]\r\ndeath_debuffs = 0\r\nx = 2\r\n");
        // A BOM on the key's own line.
        let t = "\u{feff}death_debuffs=1\n";
        assert_eq!(rewrite_ini(t, &[u("death_debuffs", "0")]), "\u{feff}death_debuffs=0\n");
    }

    #[test]
    fn commented_out_line_is_untouched_and_key_is_appended() {
        let t = "password = x\n[GAMEPLAY]\n;ingame_menu = 1\nshare_loot = 1\n\n[DISPLAY]\nvsync = 0\n";
        let r = rewrite_ini(t, &[u("ingame_menu", "0")]);
        assert_eq!(r, "password = x\n[GAMEPLAY]\n;ingame_menu = 1\nshare_loot = 1\ningame_menu = 0\n\n[DISPLAY]\nvsync = 0\n");
    }

    #[test]
    fn missing_key_without_a_gameplay_section_goes_to_the_end() {
        assert_eq!(rewrite_ini("a = 1", &[u("b", "2")]), "a = 1\nb = 2\n");
        assert_eq!(rewrite_ini("a = 1\r\n", &[u("b", "2")]), "a = 1\r\nb = 2\r\n");
        assert_eq!(rewrite_ini("", &[u("b", "2")]), "b = 2\n");
    }

    #[test]
    fn missing_key_at_the_end_of_the_gameplay_section_at_eof() {
        assert_eq!(rewrite_ini("[GAMEPLAY]\na = 1", &[u("b", "2")]), "[GAMEPLAY]\na = 1\nb = 2\n");
    }

    #[test]
    fn idempotent() {
        let t = default_template_text();
        let upd = [u("enemy_health_scaling", "77"), u("death_debuffs", "0"), u("not_there_key", "5")];
        let once = rewrite_ini(&t, &upd);
        assert_ne!(once, t);
        assert_eq!(rewrite_ini(&once, &upd), once);
        let ini = Ini::parse(&once);
        assert_eq!(ini.keys.get("enemy_health_scaling").map(String::as_str), Some("77"));
        assert_eq!(ini.keys.get("death_debuffs").map(String::as_str), Some("0"));
        assert_eq!(ini.keys.get("not_there_key").map(String::as_str), Some("5"));
    }

    fn default_template_text() -> String {
        TEMPLATE.replace("@PASSWORD@", "").replace("@NAME@", "").replace("@TRANSPORT@", "auto").replace("@HOST@", "")
    }

    #[test]
    fn new_menu_keys_are_in_the_default_file() {
        let t = default_template_text();
        let ini = Ini::parse(&t);
        assert_eq!(ini.keys.get("ingame_menu").map(String::as_str), Some("0"));
        assert_eq!(ini.keys.get("ingame_menu_key").map(String::as_str), Some("45"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_text() -> String {
        TEMPLATE.replace("@PASSWORD@", "").replace("@NAME@", "").replace("@TRANSPORT@", "auto").replace("@HOST@", "")
    }

    #[test]
    fn default_file_has_exactly_the_keys_the_reader_asks_for() {
        let ini = Ini::parse(&default_text());
        let mut warn = Vec::new();
        parse_net(&ini, &mut warn);
        apply(&ini, &mut warn);
        assert!(warn.is_empty(), "default file gives warnings: {warn:?}");
        let asked = ini.asked.borrow();
        let mut in_file: Vec<_> = ini.keys.keys().filter(|k| !asked.contains(*k)).collect();
        in_file.sort();
        assert!(in_file.is_empty(), "keys in the default file nobody reads: {in_file:?}");
        // `observe` is only asked for once at start-up (STATE is empty in this test), so it is covered.
        let mut missing: Vec<_> = asked.iter().filter(|k| !ini.keys.contains_key(*k)).collect();
        missing.sort();
        assert!(missing.is_empty(), "keys the reader asks for that the default file lacks: {missing:?}");
    }

    #[test]
    fn default_file_equals_the_built_in_defaults() {
        let before = tuning::describe();
        let ini = Ini::parse(&default_text());
        let net = parse_net(&ini, &mut Vec::new());
        apply(&ini, &mut Vec::new());
        assert_eq!(tuning::describe(), before);
        assert_eq!(net, NetSettings::default());
        assert!(fps::UNLOCK.load(Relaxed));
        assert!(fps::VSYNC_OFF.load(Relaxed));
    }

    #[test]
    fn values_parse_and_bad_ones_fall_back_with_a_warning() {
        let ini = Ini::parse("\u{feff}[NETWORK]\npassword = a = b ; c\nrole = JOIN\ntransport = steam\nudp_port = 99999\n; share_loot = 0\nvsync=1\nboss_reward = maybe\n");
        let mut warn = Vec::new();
        let net = parse_net(&ini, &mut warn);
        assert_eq!(net.password, "a = b ; c");
        assert_eq!(net.transport, TransportSetting::Steam);
        assert_eq!(net.udp_port, 65535);
        assert!(!net.auto_connect);
        assert!(ini.flag("vsync", false, &mut warn));
        assert!(ini.flag("share_loot", true, &mut warn));
        assert!(ini.flag("boss_reward", true, &mut warn));
        assert_eq!(warn.len(), 1);
    }

    /// A settings file from before the items ([NETWORK] section, `role`): every key it
    /// shares with the new layout is read, `role` is ignored without a warning.
    #[test]
    fn party_bar_settings_parse_and_fall_back() {
        let mut warn = Vec::new();
        let ini = Ini::parse("party_hp_position = TOP\nparty_hp_content = hp_numbers\nparty_hp_scale = 500\n");
        assert_eq!(ini.choice("party_hp_position", &["head", "left", "top"], PARTY_POS_HEAD, &mut warn), PARTY_POS_TOP);
        assert_eq!(ini.choice("party_hp_content", &["name_hp", "hp", "hp_numbers"], 0, &mut warn), PARTY_CONTENT_HP_NUMBERS);
        assert_eq!(ini.num("party_hp_scale", 100, 50, 200, &mut warn), 200);
        assert!(warn.is_empty(), "{warn:?}");
        let bad = Ini::parse("party_hp_position = middle\n");
        assert_eq!(bad.choice("party_hp_position", &["head", "left", "top"], PARTY_POS_HEAD, &mut warn), PARTY_POS_HEAD);
        assert_eq!(warn.len(), 1);
        let empty = Ini::parse("party_hp_position =\n");
        assert_eq!(empty.choice("party_hp_position", &["head", "left", "top"], PARTY_POS_LEFT, &mut warn), PARTY_POS_LEFT);
        assert_eq!(warn.len(), 1);
    }

    #[test]
    fn old_network_section_still_works() {
        let old = "[NETWORK]\npassword = wolf\nplayer_name = Sekiro\nrole = host\ntransport = udp\n\
                   udp_host_address = 203.0.113.7:28100\nudp_port = 28200\n\n[GAMEPLAY]\nshare_loot = 1\n";
        let mut warn = Vec::new();
        let net = parse_net(&Ini::parse(old), &mut warn);
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(net.password, "wolf");
        assert_eq!(net.player_name, "Sekiro");
        assert_eq!(net.transport, TransportSetting::Udp);
        assert_eq!(net.udp_host_address, "203.0.113.7:28100");
        assert_eq!(net.udp_port, 28200);
        assert!(!net.auto_connect);
    }

    #[test]
    fn auto_connect_and_an_empty_name() {
        let mut warn = Vec::new();
        let net = parse_net(&Ini::parse("password = x\nplayer_name =\n[ADVANCED]\nauto_connect = 1\n"), &mut warn);
        assert!(warn.is_empty(), "{warn:?}");
        assert!(net.auto_connect);
        assert_eq!(net.player_name, "");
        assert_eq!(net.transport, TransportSetting::Auto);
    }

    /// The 3-6 player keys (PLAN-MULTIPLAYER §3): in the default file with the multiplayer
    /// defaults, and parsed / clamped like the reader does. Statics are not written here
    /// (the other tests compare them against the defaults in parallel).
    #[test]
    fn multiplayer_keys() {
        let ini = Ini::parse(&default_text());
        assert_eq!(ini.text("max_players"), "6");
        assert_eq!(ini.text("relay_snapshot_hz"), "30");
        assert_eq!(ini.text("snapshot_hz_multi"), "60");
        assert_eq!(ini.text("scaling_count"), "session");
        let t = default_text();
        assert!(t.find("max_players =").unwrap() > t.find("[ADVANCED]").unwrap());
        assert!(t.find("relay_snapshot_hz =").unwrap() > t.find("[DEBUG]").unwrap());
        assert!(t.find("scaling_count =").unwrap() < t.find("[DISPLAY]").unwrap());

        let mut warn = Vec::new();
        let max = crate::peers::MAX_PLAYERS as u32;
        let ini = Ini::parse("max_players = 9\nrelay_snapshot_hz = 1\nsnapshot_hz_multi = 500\n");
        assert_eq!(ini.num("max_players", 2, 2, max, &mut warn), 6);
        assert_eq!(Ini::parse("max_players = 1").num("max_players", 2, 2, max, &mut warn), 2);
        assert_eq!(ini.num("relay_snapshot_hz", 30, 10, 120, &mut warn), 10);
        assert_eq!(ini.num("snapshot_hz_multi", 60, 20, 120, &mut warn), 120);
        assert!(warn.is_empty(), "{warn:?}");
        // loopback_peers is a network key (a change reconnects): default 0, at most 5.
        assert_eq!(parse_net(&Ini::parse(&default_text()), &mut warn).loopback_peers, 0);
        assert!(t.find("loopback_peers =").unwrap() > t.find("[DEBUG]").unwrap());
        assert_eq!(parse_net(&Ini::parse("loopback_peers = 9"), &mut warn).loopback_peers, 5);
        assert!(warn.is_empty(), "{warn:?}");

        assert_eq!(tuning::parse_scaling_count("Block", &mut warn), tuning::SCALING_COUNT_BLOCK);
        assert_eq!(tuning::parse_scaling_count("", &mut warn), tuning::SCALING_COUNT_SESSION);
        assert!(warn.is_empty(), "{warn:?}");
        assert_eq!(tuning::parse_scaling_count("nearby", &mut warn), tuning::SCALING_COUNT_SESSION);
        assert_eq!(warn.len(), 1);
    }

    /// `quest_choices_host_only`: in the default file (off), parses on / off (the setting's
    /// static is not touched here: other tests read it).
    #[test]
    fn quest_choices_host_only_is_in_the_file_and_parses() {
        let t = default_text();
        assert!(t.contains("quest_choices_host_only = 0"));
        let mut warn = Vec::new();
        assert!(!Ini::parse(&t).flag("quest_choices_host_only", true, &mut warn));
        assert!(Ini::parse("[GAMEPLAY]\nquest_choices_host_only = 1\n").flag("quest_choices_host_only", false, &mut warn));
        assert!(warn.is_empty(), "{warn:?}");
    }

    /// `share_resurrect_unlock`: in the default file (on), parses off.
    #[test]
    fn share_resurrect_unlock_is_in_the_file_and_parses() {
        let t = default_text();
        assert!(t.contains("share_resurrect_unlock = 1"));
        let mut warn = Vec::new();
        assert!(Ini::parse(&t).flag("share_resurrect_unlock", false, &mut warn));
        assert!(!Ini::parse("[GAMEPLAY]\nshare_resurrect_unlock = 0\n").flag("share_resurrect_unlock", true, &mut warn));
        assert!(warn.is_empty(), "{warn:?}");
    }

    /// The top of the generated file is what a player needs: the password and the name
    /// come before every section, [ADVANCED] and [DEBUG] are last and nothing names a role.
    #[test]
    fn default_file_layout() {
        let t = default_text();
        let at = |s: &str| t.find(s).unwrap_or_else(|| panic!("{s} missing"));
        assert!(at("password =") < at("[GAMEPLAY]"));
        assert!(at("player_name =") < at("[GAMEPLAY]"));
        assert!(at("[GAMEPLAY]") < at("[DISPLAY]") && at("[DISPLAY]") < at("[ADVANCED]") && at("[ADVANCED]") < at("[DEBUG]"));
        assert!(at("[ADVANCED]") < at("transport =") && at("auto_connect =") < at("[DEBUG]"));
        assert!(!t.contains("role ="));
        assert!(!t.contains("[NETWORK]"));
    }
}
