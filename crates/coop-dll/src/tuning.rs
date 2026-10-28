//! Switchable options, so one build can compare several fixes live. Read every frame by the
//! game task; written by the settings file reader (`settings.rs`, sections [GAMEPLAY] and [DEBUG]).

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, Ordering};

/// Ghost drawn this many ms in the past, interpolated between snapshots
/// (0 = latest snapshot as is: the old "slideshow" behaviour).
pub static GHOST_DELAY_MS: AtomicU32 = AtomicU32::new(80);
/// Turn the ghost to face where the friend faces.
pub static GHOST_ANGLE: AtomicBool = AtomicBool::new(true);
/// Mirror the friend's behavior events onto the ghost (replay-style animation).
pub static GHOST_HKB: AtomicBool = AtomicBool::new(false);
/// Old per-id W_Event animation path (only a few ids play).
pub static GHOST_WEVENT: AtomicBool = AtomicBool::new(false);

pub const ENEMY_OFF: u8 = 0;
pub const ENEMY_HP: u8 = 1;
pub const ENEMY_POS: u8 = 2;
pub const ENEMY_POS_EVENTS: u8 = 3;
/// The host's AI drives the client's enemies through the game's own
/// NetAIManipulator (RE-FINDINGS-2 § Enemy AI authority; netai.rs). Enemies
/// that cannot be switched fall back to mode-2 positions.
pub const ENEMY_NETAI: u8 = 4;
/// 0 off, 1 HP and deaths only, 2 + positions, 3 + host behavior events,
/// 4 host AI via NetAI (+ HP). Default 4 from 2026-10-22: it is the game's
/// own remote-enemy path, the client only writes records into its own
/// table, and every other mode is one click away in the menu if a live test
/// shows trouble. NOT verified live yet.
pub static ENEMY_MODE: AtomicU8 = AtomicU8::new(ENEMY_NETAI);
/// Fallback (client): enemies whose position the host drives through our
/// hook (modes 2/3, NetAI fallbacks) get ChrCtrl "CompletelyNoMove", so
/// their own AI cannot walk them away from the hooked position.
pub static ENEMY_NO_MOVE: AtomicBool = AtomicBool::new(false);
/// On every new connection the client takes the host's shared event flags
/// (bosses, doors, idols) before its join warp (world.rs).
pub static WORLD_ON_JOIN: AtomicBool = AtomicBool::new(true);
/// Client: position smoothing per frame in percent (35 = old value).
pub static ENEMY_SMOOTH_PCT: AtomicU32 = AtomicU32::new(20);

pub fn ghost_delay_ms() -> u32 {
    GHOST_DELAY_MS.load(Ordering::Relaxed)
}
pub fn ghost_angle() -> bool {
    GHOST_ANGLE.load(Ordering::Relaxed)
}
pub fn ghost_hkb() -> bool {
    GHOST_HKB.load(Ordering::Relaxed)
}
pub fn ghost_wevent() -> bool {
    GHOST_WEVENT.load(Ordering::Relaxed)
}
pub fn enemy_mode() -> u8 {
    ENEMY_MODE.load(Ordering::Relaxed)
}
pub fn enemy_smoothing() -> f32 {
    ENEMY_SMOOTH_PCT.load(Ordering::Relaxed).clamp(1, 100) as f32 / 100.0
}
/// Modes in which the client pins enemies to host positions with the hook.
/// (Mode 4 does so only for enemies NetAI cannot take.)
pub fn enemy_hook_positions() -> bool {
    matches!(enemy_mode(), ENEMY_POS | ENEMY_POS_EVENTS)
}
pub fn enemy_no_move() -> bool {
    ENEMY_NO_MOVE.load(Ordering::Relaxed)
}
pub fn world_on_join() -> bool {
    WORLD_ON_JOIN.load(Ordering::Relaxed)
}

/// One line for the log, so every bug report says which options were on.
pub fn describe() -> String {
    format!(
        "ghost delay {} ms, angle {}, hkb {}, wevent {}; enemy mode {} ({}), smoothing {}%; join reload {}; enemy delay {} ms; solid {}; motion {}; enemy no-move {}; world on join {}; native remote {} (solid {}, dress {}, spawn after {} s, dress after {} ms, N0 clones {}); death restore after {} s; boss spectate {}, expel {}, reward {}; death refill {}; max players {}, relay snapshots {} Hz, multi snapshots {} Hz, scaling count {}",
        ghost_delay_ms(),
        ghost_angle(),
        ghost_hkb(),
        ghost_wevent(),
        enemy_mode(),
        match enemy_mode() {
            ENEMY_OFF => "off",
            ENEMY_HP => "hp",
            ENEMY_POS => "pos",
            ENEMY_POS_EVENTS => "pos+events",
            ENEMY_NETAI => "netai",
            _ => "?",
        },
        ENEMY_SMOOTH_PCT.load(Ordering::Relaxed),
        join_reload(),
        enemy_delay_ms(),
        ghost_solid(),
        ghost_motion(),
        enemy_no_move(),
        world_on_join(),
        native_remote(),
        native_solid(),
        native_dress(),
        native_spawn_delay_s(),
        native_dress_delay_ms(),
        n0_clones(),
        death_restore_delay_s(),
        boss_spectate(),
        boss_expel(),
        boss_reward(),
        death_refill(),
        max_players(),
        relay_snapshot_hz(),
        snapshot_hz_multi(),
        if scaling_count() == SCALING_COUNT_BLOCK { "block" } else { "session" }
    )
}

/// The friend as a native remote PlayerIns driven by the game's own player
/// packets (native.rs, RE-FINDINGS-2 § Native remote player) instead of the
/// replay ghost. Off by default until confirmed live; the ghost stays the
/// fallback whenever the native spawn fails. Either side's setting decides
/// how that side shows the other.
pub static NATIVE_REMOTE: AtomicBool = AtomicBool::new(true);
/// Native remote (and the N0 test clone): slot-130 override, so chr type 1's
/// white-phantom shading is not applied.
pub static NATIVE_SOLID: AtomicBool = AtomicBool::new(true);
/// Native remote: copy our equipment ids onto it right after creation.
pub static NATIVE_DRESS: AtomicBool = AtomicBool::new(true);

pub fn native_remote() -> bool {
    // Only in builds with the `dev-native` feature (on by default in test builds).
    cfg!(feature = "dev-native") && NATIVE_REMOTE.load(Ordering::Relaxed)
}
pub fn native_solid() -> bool {
    NATIVE_SOLID.load(Ordering::Relaxed)
}
pub fn native_dress() -> bool {
    NATIVE_DRESS.load(Ordering::Relaxed)
}

// --- Load-time mitigations for the driver crash (gpuguard.rs, 2026-10-23) ---------------
// The crash fires most in the seconds after a load, while new models and materials are
// allocated. These keep our own extra work out of that window.

/// Native remote: created only this many seconds after the world loaded (was a fixed 6 s).
pub static NATIVE_SPAWN_DELAY_S: AtomicU32 = AtomicU32::new(10);
/// Native remote: dressed this many ms after its creation, once it is in the player chr
/// set (its model built). 0 = right after the factory, as before.
pub static NATIVE_DRESS_DELAY_MS: AtomicU32 = AtomicU32::new(3000);
/// Host after its own death reload: the friend's dead enemies are restored (ForceDead) only
/// this many seconds after the load (was a fixed 4 s).
pub static DEATH_RESTORE_DELAY_S: AtomicU32 = AtomicU32::new(8);
/// After the mod's own null-argument death reload (spectator released on a wipe) the game reads
/// emblems and sen as 0; true = give back exactly the amounts seen just before (invguard.rs).
pub static RESTORE_LOST_ITEMS: AtomicBool = AtomicBool::new(true);

/// N0 one-machine multi-player test (`[DEBUG] n0_clones`, native.rs `spawn_test`): this
/// many local loopback clones (playerNo 1..5, PGD slots 0..4) after every load. 0 = off.
pub static N0_CLONES: AtomicU32 = AtomicU32::new(0);

pub fn n0_clones() -> u32 {
    N0_CLONES.load(Ordering::Relaxed)
}

pub fn native_spawn_delay_s() -> u32 {
    NATIVE_SPAWN_DELAY_S.load(Ordering::Relaxed)
}
pub fn native_dress_delay_ms() -> u32 {
    NATIVE_DRESS_DELAY_MS.load(Ordering::Relaxed)
}
pub fn death_restore_delay_s() -> u32 {
    DEATH_RESTORE_DELAY_S.load(Ordering::Relaxed)
}

/// Join / idol-follow always through a loading screen (fresh world).
/// `[GAMEPLAY] dialog_default_yes`: the co-op yes/no box highlights Yes by default (dialog.rs
/// `open_default_yes`); false = the game's own default (No).
pub static DIALOG_DEFAULT_YES: AtomicBool = AtomicBool::new(true);
pub static JOIN_RELOAD: AtomicBool = AtomicBool::new(true);

pub fn join_reload() -> bool {
    JOIN_RELOAD.load(Ordering::Relaxed)
}

/// Client, after the join warp: rest at the idol we arrived at (silently, the host does
/// not hear of it) so enemies we had killed in our own save are alive again and the
/// host's HP mirror kills only those that are dead there. Live 2026-10-23: a warp alone
/// left the joiner's dead enemies dead while they were alive on the host.
pub static JOIN_REST: AtomicBool = AtomicBool::new(true);

pub fn join_rest() -> bool {
    JOIN_REST.load(Ordering::Relaxed)
}

/// Follow the friend's idol warps automatically (off: free travel, "go to friend" in the menu).
pub static AUTO_FOLLOW: AtomicBool = AtomicBool::new(false);

pub fn auto_follow() -> bool {
    AUTO_FOLLOW.load(Ordering::Relaxed)
}

/// Boss rules (docs/SPEC-BOSS-RULES.md). Dying in a boss fight while the friend
/// still fights: wait on the death screen (phase A of the spectator mode) until
/// the boss falls, the friend dies or rests, or the player wakes at the idol.
pub static BOSS_SPECTATE: AtomicBool = AtomicBool::new(true);
/// The friend's rest sends a player who is in a boss fight back to their idol
/// (instead of resetting the arena under them).
pub static BOSS_EXPEL: AtomicBool = AtomicBool::new(false);
/// The item lots a boss fight gave the killer are given to the friend too.
pub static BOSS_REWARD: AtomicBool = AtomicBool::new(true);

/// After a death reload the player gets a rest's HP / gourd / resurrection / item restock
/// (enemies are not touched).
pub static DEATH_REFILL: AtomicBool = AtomicBool::new(true);
pub fn death_refill() -> bool {
    DEATH_REFILL.load(Ordering::Relaxed)
}
pub fn boss_spectate() -> bool {
    BOSS_SPECTATE.load(Ordering::Relaxed)
}
pub fn boss_expel() -> bool {
    BOSS_EXPEL.load(Ordering::Relaxed)
}
pub fn boss_reward() -> bool {
    BOSS_REWARD.load(Ordering::Relaxed)
}

/// Settings file `spectate_camera` (bossrules.rs, spectate_cam.rs): what a player who
/// died in the friend's boss fight sees while waiting (needs `boss_spectate`).
/// 0 = the death screen only (phase A); 1 = no death screen, the world stays and the
/// camera follows the friend (phase B); 2 = the death screen stays, the camera follows
/// the friend behind it (phase A + camera).
pub static SPECTATE_CAMERA: AtomicU8 = AtomicU8::new(SPECTATE_CAM_PHASE_B);
pub const SPECTATE_CAM_OFF: u8 = 0;
pub const SPECTATE_CAM_PHASE_B: u8 = 1;

pub fn spectate_camera() -> u8 {
    SPECTATE_CAMERA.load(Ordering::Relaxed)
}

/// Settings file `spectate_orbit` (spectate_cam.rs, spectate_input.rs): who steers the
/// spectator camera. 0 = nobody (the old fixed camera behind the friend), 1 = the game's own
/// camera input (ChrCam still turns), 2 = XInput right stick.
pub static SPECTATE_ORBIT: AtomicU8 = AtomicU8::new(1);
/// `spectate_orbit_speed` in thousandths (1000 = 150 deg/s on the stick).
pub static SPECTATE_ORBIT_SPEED_MILLI: AtomicU32 = AtomicU32::new(1000);
/// `spectate_lock`: the lock-on toggle works.
pub static SPECTATE_LOCK: AtomicBool = AtomicBool::new(true);
/// `spectate_lock_key`: virtual-key code of the lock toggle (4 = middle mouse, 0 = none).
pub static SPECTATE_LOCK_KEY: AtomicU32 = AtomicU32::new(4);
/// `spectate_switch_key`: virtual-key code that moves the spectator camera to the next living
/// player (5 = mouse side button X1, 0 = none); D-pad right / left always do (next / previous).
pub static SPECTATE_SWITCH_KEY: AtomicU32 = AtomicU32::new(5);
/// `spectate_lock_height` in thousandths of a metre (aim point above the target's feet).
pub static SPECTATE_LOCK_HEIGHT_MILLI: AtomicU32 = AtomicU32::new(1500);
/// `spectate_recenter_s`: seconds without orbit input before the camera swings behind the
/// friend again (0 = never).
pub static SPECTATE_RECENTER_S: AtomicU32 = AtomicU32::new(0);

/// `spectate_replay_load`: 1 = a spectator released on a wipe replays the held vanilla
/// death-warp request (keeps items); 0 = the old null-arg 0xA8F7E0 reload.
pub static SPECTATE_REPLAY_LOAD: AtomicBool = AtomicBool::new(true);
pub fn spectate_replay_load() -> bool {
    SPECTATE_REPLAY_LOAD.load(Ordering::Relaxed)
}

pub fn spectate_orbit() -> u8 {
    SPECTATE_ORBIT.load(Ordering::Relaxed)
}
pub fn spectate_orbit_speed() -> f32 {
    SPECTATE_ORBIT_SPEED_MILLI.load(Ordering::Relaxed) as f32 / 1000.0
}
pub fn spectate_lock() -> bool {
    SPECTATE_LOCK.load(Ordering::Relaxed)
}
pub fn spectate_lock_key() -> i32 {
    SPECTATE_LOCK_KEY.load(Ordering::Relaxed) as i32
}
pub fn spectate_switch_key() -> i32 {
    SPECTATE_SWITCH_KEY.load(Ordering::Relaxed) as i32
}
pub fn spectate_lock_height() -> f32 {
    SPECTATE_LOCK_HEIGHT_MILLI.load(Ordering::Relaxed) as f32 / 1000.0
}
pub fn spectate_recenter_s() -> u32 {
    SPECTATE_RECENTER_S.load(Ordering::Relaxed)
}

/// Client: enemies drawn this many ms in the past, interpolated between host
/// updates (0 = the old per-frame chase with ENEMY_SMOOTH_PCT).
pub static ENEMY_DELAY_MS: AtomicU32 = AtomicU32::new(120);

pub fn enemy_delay_ms() -> u32 {
    ENEMY_DELAY_MS.load(Ordering::Relaxed)
}

/// Ghost look: 0 = game's phantom look, 1 = solid (own vtable + shader value
/// cleared every frame), 2 = 1 + chr type "other player" and re-dress.
/// Applied when a ghost is created (the shader part also live).
pub static GHOST_SOLID: AtomicU8 = AtomicU8::new(1);

pub fn ghost_solid() -> u8 {
    GHOST_SOLID.load(Ordering::Relaxed)
}

/// Replay-style ghost animation: behavior sync states + movement vector.
pub static GHOST_MOTION: AtomicBool = AtomicBool::new(true);

pub fn ghost_motion() -> bool {
    GHOST_MOTION.load(Ordering::Relaxed)
}

/// Settings file `death_debuffs`: max HP / posture lowered per co-op death (debuff.rs).
pub static DEATH_DEBUFFS: AtomicBool = AtomicBool::new(true);
/// Settings file `death_debuff_icon`: 0 = base value writes only (phase 1), 1 = own SpEffect rows
/// with an icon and attack penalty (speffect.rs), > 1 = the same with this icon id.
pub static DEATH_DEBUFF_ICON: AtomicU32 = AtomicU32::new(0);
/// Settings file `*_scaling`: percent added per extra player (scaling.rs; host's values).
pub static ENEMY_HEALTH_SCALING: AtomicU32 = AtomicU32::new(60);
pub static BOSS_HEALTH_SCALING: AtomicU32 = AtomicU32::new(50);
pub static ENEMY_POSTURE_SCALING: AtomicU32 = AtomicU32::new(60);
pub static BOSS_POSTURE_SCALING: AtomicU32 = AtomicU32::new(50);

/// Settings file `ingame_menu`: the 'Mod settings' panel beside the game's System menu (menu.rs, ui.rs).
/// Read live; 0 = no panel, no message filter, the System-list stamp is ignored.
pub static INGAME_MENU: AtomicBool = AtomicBool::new(true);
/// Settings file `ingame_menu_key`: virtual-key code (decimal) that collapses / expands the panel (45 = Insert).
pub static INGAME_MENU_KEY: AtomicU32 = AtomicU32::new(45);

/// Settings file `native_mod_menu`: 1 = replace the game's Network Settings page with the native Co-op Settings
/// page (experimental; needs the paired optmenu code). Read live; default off.
pub static NATIVE_MOD_MENU: AtomicBool = AtomicBool::new(false);
/// Settings file `native_mod_menu_inworld`: 1 = also replace the page when Options is opened inside a world
/// (crashed three times in the game's page constructor, v8.40; default off = the original page is shown in a world).
pub static NATIVE_MOD_MENU_INWORLD: AtomicBool = AtomicBool::new(true);

pub fn ingame_menu() -> bool {
    INGAME_MENU.load(Ordering::Relaxed)
}

pub fn ingame_menu_key() -> u32 {
    INGAME_MENU_KEY.load(Ordering::Relaxed)
}

/// Settings file `ingame_menu_pad`: the Mod settings panel can be driven with the controller
/// (right stick, triggers, R3, LB / RB; never the D-pad, A or B). 0 = mouse and keyboard only.
pub static INGAME_MENU_PAD: AtomicBool = AtomicBool::new(true);

pub fn ingame_menu_pad() -> bool {
    INGAME_MENU_PAD.load(Ordering::Relaxed)
}

/// Settings file `debug_hotkeys`: F8 wake at the idol (boss death-screen wait), F9 go to the
/// friend, F11 log a MARK line. Off for players.
pub static DEBUG_HOTKEYS: AtomicBool = AtomicBool::new(false);

/// Settings file `options_probe` ([DEBUG], default on in v8.34a): log-only probe of the Options menu (optprobe.rs).
pub static OPTIONS_PROBE: AtomicBool = AtomicBool::new(true);

/// Settings file `f11_pad_button`: XInput button mask that also triggers the F11 mark
/// (0x0020 Back, 0x0010 Start, 0x0040 L3, 0x0080 R3; 0 = off).
pub static F11_PAD_MASK: AtomicU16 = AtomicU16::new(0x0020);

pub fn debug_hotkeys() -> bool {
    DEBUG_HOTKEYS.load(Ordering::Relaxed)
}

/// Settings file `sync_art_effects`: the friend's combat art / prosthetic bullets
/// (wind slashes, spiral cloud, shuriken...) appear on their native remote, damage-free
/// (artfx.rs). Also makes the native remote's hits on enemies reaction-only.
pub static SYNC_ART_EFFECTS: AtomicBool = AtomicBool::new(true);

pub fn sync_art_effects() -> bool {
    SYNC_ART_EFFECTS.load(Ordering::Relaxed)
}

/// Settings file `merge_quests` (ownworld.rs, NOT verified live): NPC quest state changed
/// in the shared world carries over into the joiner's own world when the session ends.
/// 0 = off (NPC quests back to the joiner's own state), 1 = per NPC unit (20-flag NPC
/// state blocks in 1000-1999, 100-flag blocks of `7xxxxxxx`), 2 = per whole 1000-flag
/// record (coarser: fewer mixed states, more of the host's NPC state).
pub static MERGE_QUESTS: AtomicU8 = AtomicU8::new(1);

pub fn merge_quests() -> u8 {
    MERGE_QUESTS.load(Ordering::Relaxed).min(2)
}

/// Settings file `sync_posture` ([GAMEPLAY], NOT verified live): the friend's
/// posture damage and deathblows reach the host's enemies, and the host's
/// posture bar is mirrored back (posture.rs).
pub static SYNC_POSTURE: AtomicBool = AtomicBool::new(true);

pub fn sync_posture() -> bool {
    SYNC_POSTURE.load(Ordering::Relaxed)
}

/// Settings file `custom_icons` ([DISPLAY], NOT verified live): the co-op items' own icons
/// are written into the menu atlas textures while they load (icons.rs). Read from the file
/// at process start (the textures load before `settings::init`); later changes only affect
/// textures loaded after them, so in practice a restart is needed.
pub static CUSTOM_ICONS: AtomicBool = AtomicBool::new(true);

pub fn custom_icons() -> bool {
    CUSTOM_ICONS.load(Ordering::Relaxed)
}

/// Settings file `native_session` ([ADVANCED], the main enemy path since 2026-10-23,
/// NOT verified live): the game's own online code over our link (nsession.rs), with
/// our own NetAI sync (enemy_mode 4) as the fallback. Off = nothing of it is installed.
/// Both players must use the same value and stage.
pub static NATIVE_SESSION: AtomicBool = AtomicBool::new(true);
/// `native_session_stage`: 1 = hooks + packet counters only, 2 = + the game's own
/// enemy ownership / movement / HP and its hit packet 21, 3 = + its small player
/// packets (28, 52, 18, 42), 4 = + ObjAct events (45, experimental).
pub static NATIVE_SESSION_STAGE: AtomicU8 = AtomicU8::new(3);

// --- 3-6 players (PLAN-MULTIPLAYER, NOT verified live) -------------------------------------

/// Settings file `max_players` ([ADVANCED]): players in the host's world, the host
/// included (2..=6, `peers::MAX_PLAYERS`). Lobby size and the session's slot limit
/// (steam.rs, coop-net `set_max_peers(max_players - 1)`); read when the world opens.
pub static MAX_PLAYERS: AtomicU8 = AtomicU8::new(6);
/// `relay_snapshot_hz` ([DEBUG]): host, the rate at which one client's snapshots are
/// forwarded to the other clients.
pub static RELAY_SNAPSHOT_HZ: AtomicU32 = AtomicU32::new(30);
/// `snapshot_hz_multi` ([DEBUG]): our own snapshot rate while more than one peer is
/// connected (with one peer it stays one per frame, net.rs SEND_EVERY).
pub static SNAPSHOT_HZ_MULTI: AtomicU32 = AtomicU32::new(60);
/// `scaling_count` ([GAMEPLAY]): which players the enemy scaling counts (host decides).
pub static SCALING_COUNT: AtomicU8 = AtomicU8::new(SCALING_COUNT_SESSION);
/// Every connected player (1 + peers).
pub const SCALING_COUNT_SESSION: u8 = 0;
/// Only players in the host's map block.
pub const SCALING_COUNT_BLOCK: u8 = 1;

pub fn max_players() -> u8 {
    MAX_PLAYERS.load(Ordering::Relaxed).clamp(2, crate::peers::MAX_PLAYERS as u8)
}
pub fn relay_snapshot_hz() -> u32 {
    RELAY_SNAPSHOT_HZ.load(Ordering::Relaxed)
}
pub fn snapshot_hz_multi() -> u32 {
    SNAPSHOT_HZ_MULTI.load(Ordering::Relaxed)
}
pub fn scaling_count() -> u8 {
    SCALING_COUNT.load(Ordering::Relaxed)
}

/// `scaling_count = session | block` (also 0 / 1).
pub fn parse_scaling_count(v: &str, warn: &mut Vec<String>) -> u8 {
    match v.trim().to_lowercase().as_str() {
        "" | "session" | "0" => SCALING_COUNT_SESSION,
        "block" | "1" => SCALING_COUNT_BLOCK,
        other => {
            warn.push(format!("scaling_count = {other:?} is not session/block, using session"));
            SCALING_COUNT_SESSION
        }
    }
}

pub fn native_session() -> bool {
    NATIVE_SESSION.load(Ordering::Relaxed)
}
pub fn native_session_stage() -> u8 {
    NATIVE_SESSION_STAGE.load(Ordering::Relaxed).clamp(1, crate::nsession::MAX_STAGE)
}
