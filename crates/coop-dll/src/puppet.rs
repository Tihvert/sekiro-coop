//! Phase 1a puppet spike (FEASIBILITY.md §8.3).
//!
//! F7  scan loaded characters by entity id, log the nearest ones, and turn the
//!     nearest living one into a puppet (no attacks, can't be hit; AI stays on).
//! F8  toggle "follow": every frame the puppet is placed 2 m from the player.
//! F6  spawn a player-model ghost (ReplayGhostIns without replay data) as the puppet.
//! F9  replay-data ghost type 3 (for comparison; the game deletes it at once).
//! F10 RE: write the decrypted in-memory sekiro.exe to %LOCALAPPDATA%sekiro-coopsekiro_dump.exe.
//! F11 RE: hex dump of CSDebugChrCreator (WorldChrMan+0x3148).
//! F5  spawn an Ashina soldier (c1010) via CSDebugChrCreator; it becomes the puppet.
//! F12 RE: ask CSDebugChrCreator to spawn c0000 (Wolf) 3 m from the player.
//!
//! Go criteria: a second character visible next to the player, AI off, moves
//! with the player, 10 minutes without a crash.

use crate::game::{self, Chr};
use crate::peers::{PeerFrame, PeerId, PeerSet};
use crate::{anim, bossrules, crash, dump, enemies, fps, ghost, hook, join, log, net, rtti, settings, ui, world};

/// `[GAMEPLAY] scale_all_loaded` (v7.7, live bisect 2026-10-24): 2-player scaling over
/// every loaded EnemyIns in the 1000000..2600000 entity range each second. 0 (default) =
/// v7.6: the enemies.rs scan list only.
pub static SCALE_ALL_LOADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

const VK_F9: i32 = 0x78;
const DEV_HOTKEYS: bool = false;
const VK_F10: i32 = 0x79;
const VK_F11: i32 = 0x7A;
const VK_F12: i32 = 0x7B;
const VK_F5: i32 = 0x74;
const VK_F6: i32 = 0x75;
const VK_F7: i32 = 0x76;
const VK_F8: i32 = 0x77;

// Map entity ids in Sekiro are 7 digits (e.g. 1100850, 1110800).
const SCAN_FIRST: i32 = 1_000_000;
const SCAN_LAST: i32 = 2_600_000;
const SCAN_PER_FRAME: i32 = 10_000;
const MIN_PUPPET_DISTANCE: f32 = 2.0;
const MAX_PUPPET_DISTANCE: f32 = 60.0;
const FOLLOW_OFFSET_X: f32 = 2.0;

#[link(name = "user32")]
unsafe extern "system" {
    fn GetAsyncKeyState(vk: i32) -> i16;
}

#[derive(Default)]
struct Key {
    was_down: bool,
}

impl Key {
    fn pressed(&mut self, vk: i32) -> bool {
        let down = unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000 != 0;
        let edge = down && !self.was_down;
        self.was_down = down;
        edge
    }
}

#[derive(Default)]
pub struct Puppet {
    f9: Key,
    f11: Key,
    pad_f11_was_down: bool,
    f10: Key,
    f12: Key,
    spawn_watch: Option<(rtti::SpawnRequest, u32)>,
    spawned: Option<Chr>,
    null_guard_logged: u32,
    in_world_since: Option<std::time::Instant>,
    translucent: bool,
    auto_ghost_try: Option<std::time::Instant>,
    remote_seen_at: Option<std::time::Instant>,
    probe: anim::Probe,
    friend_tags: Vec<ui::FriendTag>,
    /// Party HP bar rows and each peer's last non-hidden HP / max HP (kept through loads).
    party: Vec<ui::PartyMember>,
    party_last_hp: std::collections::BTreeMap<PeerId, (i32, i32)>,
    notice: Option<String>,
    enemies: enemies::Enemies,
    world: world::World,
    boss: bossrules::BossRules,
    /// Cutscene state, tracker and re-placement (cutscene.rs).
    cutscene: crate::cutscene::Cutscene,
    activation_log_at: Option<std::time::Instant>,
    /// N0 clones: the FPS line every 10 s.
    n0_log_at: Option<std::time::Instant>,
    debuff: crate::debuff::Debuff,
    scaling: crate::scaling::Scaling,
    scale_at: Option<std::time::Instant>,
    scale_remote: Option<crate::scaling::ScaleCfg>,
    /// Host: the scaling config each client's link got last (its epoch, the config).
    scale_sent: std::collections::BTreeMap<PeerId, (u64, crate::scaling::ScaleCfg)>,
    /// The game's System list is open ('Mod settings' panel, menu.rs) and since when.
    menu_open: bool,
    menu_since: Option<std::time::Instant>,
    #[cfg(feature = "testsync")]
    observe_log_at: Option<std::time::Instant>,
    /// v8.61: the 30 s link line (ping), so lag can be told from the network in the log.
    net_line_at: Option<std::time::Instant>,
    /// The join/leave notice (ui.rs `presence`) or another event text shown now:
    /// text, when it appeared, and for how many seconds it stays.
    event_notice: Option<(String, std::time::Instant, u64)>,
    #[cfg(feature = "testsync")]
    testsync: crate::testsync::TestSync,
    motion: crate::motion::Motion,
    phantom_log_at: Option<std::time::Instant>,
    hkb: crate::hkbsync::HkbSync,
    tuning_logged: bool,
    modules_logged: u32,
    anims_ok: u32,
    last_anim: i32,
    anims_played: u64,
    ghost_died: bool,
    join: join::Join,
    null_guard_log_at: Option<std::time::Instant>,
    f5: Key,
    f6: Key,
    f7: Key,
    f8: Key,
    scan_cursor: Option<i32>,
    found: Vec<(i32, Chr)>,
    puppet_entity: Option<i32>,
    follow: bool,
    follow_frames: u64,
    net: Option<Option<net::Net>>,
    fps_frames: u32,
    fps_since: Option<std::time::Instant>,
    fps: f32,
    /// The player ChrIns seen last frame: a different one means the world was
    /// rebuilt even if no frame without a player was seen.
    last_player: Option<Chr>,
    /// We called Warp (join/follow): hands off every character until the
    /// load it starts has been seen (or WARP_HANDS_OFF passed).
    warp_pending: Option<std::time::Instant>,
    /// The friend (or the N0 test clone) as a native remote PlayerIns.
    native: crate::native::Native,
    /// Every other player this frame (peers.rs, PLAN-MULTIPLAYER §5.3), rebuilt once per
    /// frame by `build_peers`; handed to the modules alongside their old friend arguments
    /// until P1c/P1d switch them over.
    peers: PeerSet,
    /// The peer ids last logged (a line when the set changes).
    peers_logged: Vec<PeerId>,
}

/// After our own Warp call the old world is torn down during the fade-out,
/// possibly while the player pointer is still set. Touch no character until
/// the loading screen (no player) is seen, at most this long.
const WARP_HANDS_OFF: std::time::Duration = std::time::Duration::from_secs(20);

fn is_ghost_ins(c: Chr) -> bool {
    rtti::class_name(c.0).is_some_and(|n| n.contains("ReplayGhostIns"))
}

impl Puppet {
    /// Every game pointer we keep across frames belongs to one loaded world.
    /// Called on anything that may rebuild it (loading screen, player
    /// re-created, our own warp): unkeys the whole hook table (ghost AND
    /// enemy slots) and forgets the ghost, enemies, hkb queues and pending
    /// spawns. Everything is re-resolved by entity id or re-created once the
    /// new world has settled. Raising hkb events on, or writing the angle
    /// into, a ghost the game had already freed corrupts the heap, which
    /// surfaces seconds later somewhere else (2026-10-22: NVIDIA driver crash
    /// ~3 s after a ghost appeared right after an idol warp).
    /// Host-driven (NetAI) enemies are only forgotten here, never written
    /// back: their objects die with the world (enemies.rs `reset`).
    fn invalidate_world(&mut self, why: &str) {
        hook::clear_all_targets();
        if self.spawned.take().is_some() {
            log::line(&format!("spawn: {why}, spawned puppet forgotten"));
        }
        self.enemies.reset();
        self.hkb.on_world_reset();
        // The native remote is only forgotten (and deleted / its PGD slot
        // freed once the new world has settled, native.rs `tick`).
        self.native.forget(why);
        // Native session (nsession.rs): every patch out before the world goes (no-op when off).
        crate::nsession::on_world_reset(why);
        crate::restsync::on_world_reset();
        self.friend_tags.clear();
        ui::publish_tags(Vec::new());
        self.ghost_died = false;
        self.last_anim = 0;
        self.in_world_since = None;
        self.spawn_watch = None;
        self.scan_cursor = None;
        self.found.clear();
    }

    /// We called Warp (join, follow, expulsion). The ghost is still valid this
    /// frame: ask the game to remove it (if the warp does not reload after all,
    /// no frozen copy is left behind), then let go of everything until the load.
    fn after_warp(&mut self) {
        if let Some(g) = self.spawned.filter(|&c| is_ghost_ins(c)) {
            ghost::despawn(g);
        }
        self.invalidate_world("our warp");
        self.warp_pending = Some(std::time::Instant::now());
    }

    /// The 'Mod settings' panel (menu.rs, ui.rs), every frame: the 'System list open' latch,
    /// the host's edit (applied, saved to the ini, sent on by the ScaleCfg resend) and the
    /// values the render thread draws. The render thread never touches the game or the file.
    fn menu_frame(&mut self, player_now: Option<Chr>) {
        use crate::menu::{self, Role, Values};
        let enabled = crate::tuning::ingame_menu();
        let role = match self.net.as_ref() {
            Some(Some(n)) if n.connected() => {
                if n.is_host() {
                    Role::Host
                } else {
                    Role::Client
                }
            }
            _ => Role::Solo,
        };
        // The latch. The game is only asked while a stamp is fresh or the panel is up.
        if enabled {
            let age = crate::quitblock::system_list_age_ms();
            if self.menu_open || age.is_some_and(|a| a < menu::STAMP_FRESH_MS) {
                let input = menu::LatchIn {
                    stamp_age_ms: age,
                    mask: game::menu_mask(),
                    load_pending: game::load_pending(),
                    in_world: player_now.is_some(),
                    dead: player_now.and_then(|p| p.hp()).is_none_or(|hp| hp <= 0),
                    held_ms: self.menu_since.map_or(0, |t| t.elapsed().as_millis() as u64),
                };
                let (open, why) = menu::latch_step(self.menu_open, input);
                if open != self.menu_open {
                    self.menu_open = open;
                    self.menu_since = open.then(std::time::Instant::now);
                    match (open, why) {
                        (true, _) => log::line("menu: system list open"),
                        (false, why) => log::line(&format!("menu: closed ({})", why.unwrap_or("?"))),
                    }
                }
            }
        } else if self.menu_open {
            self.menu_open = false;
            self.menu_since = None;
            log::line("menu: closed (ingame_menu = 0)");
        }
        // The host's edit. A client is refused here too (the panel disables its widgets).
        if let Some(v) = menu::take_edit() {
            if !enabled && !crate::tuning::NATIVE_MOD_MENU.load(std::sync::atomic::Ordering::Relaxed) {
                log::line("menu: edit ignored (ingame_menu = 0)");
            } else if !menu::can_edit(role) {
                log::line("menu: edit refused (client)");
                menu::set_save_result("Not changed: only the host can change these.".into());
            } else {
                use std::sync::atomic::Ordering::Relaxed;
                let c = |x: u16| menu::clamp_pct(x as i64);
                let v = Values { enemy_hp: c(v.enemy_hp), enemy_posture: c(v.enemy_posture), boss_hp: c(v.boss_hp), boss_posture: c(v.boss_posture), ..v };
                crate::tuning::ENEMY_HEALTH_SCALING.store(v.enemy_hp as u32, Relaxed);
                crate::tuning::ENEMY_POSTURE_SCALING.store(v.enemy_posture as u32, Relaxed);
                crate::tuning::BOSS_HEALTH_SCALING.store(v.boss_hp as u32, Relaxed);
                crate::tuning::BOSS_POSTURE_SCALING.store(v.boss_posture as u32, Relaxed);
                crate::tuning::DEATH_DEBUFFS.store(v.death_debuffs, Relaxed);
                let saved = settings::write_keys(&menu::ini_updates(v));
                let state = match &saved {
                    Ok(()) => "saved".to_string(),
                    Err(e) => format!("not saved ({e})"),
                };
                log::line(&format!(
                    "menu: host set enemy_hp={} enemy_posture={} boss_hp={} boss_posture={} death_debuffs={} {state}",
                    v.enemy_hp, v.enemy_posture, v.boss_hp, v.boss_posture, v.death_debuffs
                ));
                menu::set_save_result(match saved {
                    Ok(()) => "Applied and saved to the settings file.".into(),
                    Err(e) => format!("Applied, but NOT saved: {e}"),
                });
            }
        }
        let mine = menu_values();
        let host = if role == Role::Client {
            self.scale_remote.map(|c| Values {
                enemy_hp: c.enemy_hp,
                enemy_posture: c.enemy_posture,
                boss_hp: c.boss_hp,
                boss_posture: c.boss_posture,
                death_debuffs: c.debuffs.unwrap_or(mine.death_debuffs),
            })
        } else {
            None
        };
        menu::publish(role, mine, host, enabled && self.menu_open);
    }

    pub fn on_frame(&mut self) {
        crate::native::begin_frame();
        // Debug hotkeys are off in player builds: the friend hit F10 by accident
        // (a 70 MB dump) and F12 is Steam's screenshot key. The developer
        // menu buttons still work.
        let keys = DEV_HOTKEYS;
        let f9 = self.f9.pressed(VK_F9) && keys;
        let mut f11_down = self.f11.pressed(VK_F11);
        let pad_mask = crate::tuning::F11_PAD_MASK.load(std::sync::atomic::Ordering::Relaxed);
        if pad_mask != 0 || self.pad_f11_was_down {
            let buttons = if pad_mask != 0 { crate::spectate_input::pad_buttons_down() } else { None };
            let (down, pad_edge) = crate::spectate_input::pad_edge(self.pad_f11_was_down, buttons, pad_mask);
            self.pad_f11_was_down = down;
            if pad_edge {
                log::line("input: gamepad button triggered F11");
                crate::optprobe::note_pad_f11();
                f11_down = true;
            }
        }
        let f11 = f11_down && keys;
        // state_log test build: F11 = the full player state once (statelog.rs).
        let f11_state = f11_down && crate::statelog::enabled();
        let f10 = self.f10.pressed(VK_F10) && keys;
        let f12 = self.f12.pressed(VK_F12) && keys;
        let f7 = self.f7.pressed(VK_F7) && keys;
        let f8 = self.f8.pressed(VK_F8) && keys;

        // The settings file: re-read when it changed (fps and the other live settings apply
        // here); new network keys reconnect.
        settings::tick();
        fps::apply();
        let cmd = ui::take_commands();
        let f5 = self.f5.pressed(VK_F5) && keys;
        let f6 = self.f6.pressed(VK_F6) && keys;
        if settings::take_net_changed() {
            if let Some(Some(n)) = self.net.as_mut() {
                n.quit();
            }
            hook::clear_puppet();
            self.net = None;
            log::line("net: network settings changed, reconnecting");
        }
        self.fps_frames += 1;
        let since = self.fps_since.get_or_insert_with(std::time::Instant::now);
        if since.elapsed().as_secs_f32() >= 1.0 {
            self.fps = self.fps_frames as f32 / since.elapsed().as_secs_f32();
            self.fps_frames = 0;
            *since = std::time::Instant::now();
        }

        // Networking runs even on loading screens (sends "hidden" snapshots),
        // so the session never drops during loads, deaths or warps.
        // One read per frame, so the snapshot we send and everything below
        // agree on whether we are in the world.
        let player_now = game::player();
        if crate::statelog::enabled() {
            let anim = player_now.and_then(|p| self.probe.current(p)).unwrap_or(0);
            crate::statelog::tick(anim);
            if f11_state {
                log::line(&format!("MARK F11 (state dump) at {} local", log::local_hms()));
                crate::statelog::dump(anim);
            }
        }
        // F11: the remotes' raw look next to our own (native.rs), for "the friend cannot see me".
        if f11_state && let Some(p) = player_now {
            self.native.f11_dump(p);
        }
        let net = self.net.get_or_insert_with(net::Net::from_config);
        if let Some(n) = net {
            crash::breadcrumb("net frame");
            let anim = player_now.and_then(|p| self.probe.current(p)).unwrap_or(0);
            n.on_frame(player_now, anim, &mut self.motion);
            // Auditor messages leave the net inbox every frame, loads included (audit.rs).
            crate::audit::pump(n);
        }
        crash::breadcrumb("after net frame");
        let remote = net.as_ref().and_then(|n| n.remote());
        // Where the ghost is drawn this frame (interpolated a little in the
        // past: 30 Hz snapshots shown as they arrive looked like a slideshow,
        // test 2026-10-22). Computed once, so the hook target and the
        // nameplate use the same point: the tag used to read the ghost's
        // physics position at FrameBegin, i.e. last frame's target, and
        // trailed the body by a frame.
        let smoothed = net.as_ref().and_then(|n| n.remote_smoothed(crate::tuning::ghost_delay_ms()));
        // "<name> joined" / "<name> left" per peer for a few seconds on the overlay (ui.rs).
        let present: Vec<(PeerId, String)> = net
            .as_ref()
            .filter(|n| n.connected())
            .map(|n| n.peers().map(|p| (p.id(), if p.name().is_empty() { "Friend" } else { p.name() }.to_string())).collect())
            .unwrap_or_default();
        if let Some(text) = ui::presence(&present) {
            self.event_notice = Some((text, std::time::Instant::now(), 6));
        }
        for (text, secs) in bossrules::take_notices() {
            self.event_notice = Some((text, std::time::Instant::now(), secs));
        }
        if self.event_notice.as_ref().is_some_and(|(_, t, secs)| t.elapsed().as_secs() >= *secs) {
            self.event_notice = None;
        }
        // The menu only needs a few updates per second.
        if self.fps_frames % 10 == 0 {
            ui::publish(ui::Status {
                net: net.as_ref().map_or_else(net::offline_status, |n| n.status_text()),
                friend_tags: self.friend_tags.clone(),
                party: self.party.clone(),
                notice: {
                    let other = self
                        .event_notice
                        .as_ref()
                        .map(|(t, ..)| t.clone())
                        .or_else(|| self.notice.clone())
                        .or_else(|| self.testsync_notice());
                    // Waiting on the death screen comes first and stays up.
                    match (self.boss.overlay(), other) {
                        (Some(wait), Some(other)) => Some(format!("{wait}\n{other}")),
                        (Some(wait), None) => Some(wait.to_string()),
                        (None, other) => other,
                    }
                },
            });
        }
        self.menu_frame(player_now);

        // Loading screens and menus: no player, nothing else to do. A spawned
        // puppet does not survive a load, so forget it.
        let Some(player) = player_now else {
            // Once per load (the first frame without a player).
            if self.last_player.is_some() || self.in_world_since.is_some() {
                self.invalidate_world("area load");
            }
            self.last_player = None;
            // The set still follows the link on loading screens (no native remote: the
            // world is gone).
            self.build_peers(false);
            self.warp_pending = None;
            self.join.on_loading();
            self.boss.on_loading();
            self.cutscene.on_loading();
            crate::talklock::on_loading();
            self.scaling.reset();
            // Own world (ownworld.rs): an armed restore is written during this loading screen.
            crate::ownworld::on_loading();
            return;
        };
        if self.last_player.is_some_and(|p| p != player) {
            log::line("world: player re-created without a loading frame, treating it as a load");
            self.invalidate_world("player re-created");
            self.warp_pending = None;
            self.join.on_loading();
            self.boss.on_loading();
            self.cutscene.on_loading();
            crate::talklock::on_loading();
            self.scaling.reset();
        }
        self.last_player = Some(player);
        // At most one line per 10 s (per-frame logging cost FPS, 2026-10-22).
        let in_world_since = *self.in_world_since.get_or_insert_with(std::time::Instant::now);
        self.probe.on_frame(player);
        if self.warp_pending.is_some_and(|t| t.elapsed() >= WARP_HANDS_OFF) {
            log::line("join: no loading screen within 20 s of our warp, resuming");
            self.warp_pending = None;
        }
        let hands_off = self.warp_pending.is_some();
        let here = game::current_block();
        // The friend's native remote (validated in native.rs `tick` last
        // frame; loads forget it before this point).
        let native_friend = self.native.net_remote();
        // The frame's PeerSet: after net.on_frame and the native remote, before every module.
        self.build_peers(true);
        // Cutscene edges, the peers' cutscenes for the waits (cutscene.rs), the re-placement.
        self.cutscene.tick(crate::cutscene::Frame { player, peers: &self.peers, here });

        // Every peer's native remote onto its snapshot position (native.rs pins).
        self.native.follow_peers(self.net.as_ref().and_then(Option::as_ref));

        // Host: enemies near any living peer in our block get every-frame AI (activation.rs).
        let hosting = matches!(self.net.as_ref(), Some(Some(n)) if n.connected() && n.is_host());
        // Spectating (v8.6, any role, native session too): the fight we watch is far from
        // our dead player by the game's distance rule, so the boss ran every 5th / 30th
        // frame here: a slideshow on the spectator camera (live 2026-10-25, Genichiro).
        let spectating = crate::bossrules::spectating();
        let peers_here: Vec<[f32; 3]> = if spectating || (hosting && !hands_off && !crate::nsession::step_aside::activation_off()) {
            self.peers
                .iter()
                .filter_map(|p| p.snapshot)
                .filter(|r| r.hp > 0 && here == Some(r.block) && game::sane_pos(r.pos))
                .map(|r| r.pos)
                .collect()
        } else {
            Vec::new()
        };
        crate::activation::set_peers(&peers_here);
        let forced: Vec<usize> = if spectating { self.native.remotes().map(|(_, c)| c.0).collect() } else { Vec::new() };
        crate::activation::set_forced_remotes(&forced);
        if (hosting || spectating) && self.activation_log_at.is_none_or(|t| t.elapsed().as_secs() >= 10) {
            self.activation_log_at = Some(std::time::Instant::now());
            crate::activation::log_stats(&self.native.remotes().map(|(_, c)| c.0).collect::<Vec<_>>());
        }
        // Death debuff (settings death_debuffs) and enemy scaling by player count.
        // Not while our warp tears the world down: no character may be touched then.
        let linked = matches!(self.net.as_ref(), Some(Some(n)) if n.connected());
        if !hands_off {
            let own_rest = std::mem::take(&mut self.world.own_rest_now);
            self.debuff.on_frame(
                // The host's death_debuffs wins on a client (ScaleCfg 10th byte, v8.31a).
                self.scale_remote
                    .and_then(|c| c.debuffs)
                    .unwrap_or_else(|| crate::tuning::DEATH_DEBUFFS.load(std::sync::atomic::Ordering::Relaxed)),
                linked,
                own_rest,
                DEBUFF_PCT,
            );
        }
        if !linked {
            self.scale_remote = None;
        }
        if !hands_off && self.scale_at.is_none_or(|t| t.elapsed().as_secs() >= 1) {
            crash::breadcrumb("scaling");
            self.scale_at = Some(std::time::Instant::now());
            let is_host = matches!(self.net.as_ref(), Some(Some(n)) if n.is_host());
            // The host's percentages and player count on every machine (a client waits for them).
            let cfg = if is_host { Some(own_scale_cfg(scale_players(&self.peers, here))) } else { self.scale_remote };
            let mut bosses = Vec::new();
            if let Some(b) = game::boss_state() {
                bosses.push(b.entity);
            }
            for (_, e, _) in self.world.peer_bosses() {
                bosses.push(e);
            }
            let players = match cfg {
                Some(c) if linked => u32::from(c.players),
                _ => 1,
            };
            // Every loaded enemy on both roles (§35): the 45 s `known` scan had 166 of 224
            // here for minutes, so a miniboss stayed unscaled on the host only (max 2149 vs
            // 5373 on the friend) and later healed by the growth. Idempotent per chr.
            let mut all: Vec<(i32, game::Chr)> = if SCALE_ALL_LOADED.load(std::sync::atomic::Ordering::Relaxed) {
                // Only map enemies (the scan range): never an event / helper chr outside it.
                crate::nsession::enemy_chrs()
                    .into_iter()
                    .filter_map(|c| Some((c.entity_id().filter(|e| (SCAN_FIRST..=2_600_000).contains(e))?, c)))
                    .collect()
            } else {
                Vec::new()
            };
            if all.is_empty() {
                all = self.enemies.loaded_enemies(is_host);
            }
            self.scaling.apply(all.into_iter(), players, cfg, &bosses);
        }

        #[cfg(feature = "testsync")]
        if settings::observe_at_start() && self.observe_log_at.is_none_or(|t| t.elapsed().as_secs() >= 10) {
            self.observe_log_at = Some(std::time::Instant::now());
            crate::observe::log_stats();
        }

        #[cfg(feature = "testsync")]
        if matches!(self.net.as_ref(), Some(Some(n)) if n.serves_updates()) {
            self.testsync.ensure_server();
        }

        // Host-authoritative enemy sync while the link is up.
        if let Some(Some(n)) = self.net.as_mut()
            && n.connected()
        {
            if !hands_off {
                crash::breadcrumb("enemy sync");
                // The chr our enemies may target as "the friend".
                let ghost = self.spawned.filter(|&c| is_ghost_ins(c)).or(native_friend);
                self.enemies.on_frame(n, &self.peers, player, ghost, &self.probe);
            }
            if self.net_line_at.is_none_or(|t| t.elapsed().as_secs() >= 30) {
                self.net_line_at = Some(std::time::Instant::now());
                log::line(&format!("net: link: {}", n.status_text()));
            }
            // Flags only touch the flag manager (checked ready): keeps running
            // through the warp fade so nothing set right before it is lost.
            crash::breadcrumb("world sync");
            self.world.on_frame(n, &self.peers, in_world_since.elapsed());
            // An EV_DEATH taken just now reaches persist / death_sync this frame, as in v7.5.
            refresh_peer_deaths(&mut self.peers, &self.world);
            // Rest = reset for both (restsync.rs): check the respawn, the no-kill window,
            // kill bookkeeping and the native session after every rest of ours.
            if !hands_off {
                crash::breadcrumb("restsync");
                crate::restsync::tick(&mut self.enemies, &self.peers);
                // Lives / death the same on both machines under the native session (§28).
                crash::breadcrumb("livesync");
                crate::livesync::tick(&self.enemies, &self.peers);
                // Dead set, paid kills, boss records: until a rest or a wipe (§31).
                crash::breadcrumb("persist");
                crate::persist::tick(crate::persist::Ctx {
                    net: n,
                    peers: &self.peers,
                    enemies: &self.enemies,
                    player,
                    in_world_for: in_world_since.elapsed(),
                });
                // Auditor: what one player got and another did not (§32, per peer: PLAN-MULTIPLAYER
                // §5.19); observes only.
                crash::breadcrumb("audit");
                crate::audit::tick(crate::audit::Ctx {
                    net: n,
                    peers: &self.peers,
                    player,
                    here,
                    in_world_for: in_world_since.elapsed(),
                });
            }
            crash::breadcrumb("death sync, scaling cfg");
            death_sync(&mut self.world, &self.enemies, n, &self.peers, player, in_world_since);
            // Scaling: the host decides (percentages and players = 1 + peers, PLAN §2) and
            // sends it to each client on its link and on every change; a client uses only
            // the host's.
            if n.is_host() {
                let cfg = own_scale_cfg(scale_players(&self.peers, here));
                let links: Vec<(PeerId, u64)> = self.peers.link_epochs();
                self.scale_sent.retain(|id, _| links.iter().any(|(l, _)| l == id));
                for (id, e) in links {
                    if self.scale_sent.get(&id) != Some(&(e, cfg))
                        && n.send_bulk_to(crate::net::Dest::Peer(id), BULK_SCALE_CFG, &cfg.encode())
                    {
                        self.scale_sent.insert(id, (e, cfg));
                        log::line(&format!("scale: sent {cfg:?} to peer {}", id.slot()));
                    }
                }
            } else {
                while let Some((from, d)) = n.take_bulk_from(BULK_SCALE_CFG) {
                    if !from.is_host() {
                        continue;
                    }
                    self.scale_remote = crate::scaling::ScaleCfg::decode(&d);
                    log::line(&format!("scale: host uses {:?}", self.scale_remote));
                }
            }
            #[cfg(feature = "testsync")]
            self.testsync.on_frame(n);
            if !hands_off {
                crash::breadcrumb("hkb sync");
                let ghost = self.spawned.filter(|&c| is_ghost_ins(c));
                self.hkb.on_frame(n, &self.peers, player, ghost, &self.enemies);
            }
            if !self.tuning_logged {
                self.tuning_logged = true;
                log::line(&format!("tuning: {}", crate::tuning::describe()));
            }
            // Nameplates: one per peer alive and in our map block, at the exact
            // point its ghost is put this frame.
            // A native remote is moved by the engine: tag its own position.
            self.friend_tags = if hands_off {
                Vec::new()
            } else {
                self.peers
                    .iter()
                    .filter_map(|p| {
                        let r = p.snapshot.filter(|r| !r.hidden && r.hp > 0 && here == Some(r.block))?;
                        let native_pos = p.native.and_then(|c| c.position()).filter(|&q| game::sane_pos(q));
                        Some(ui::FriendTag {
                            id: p.id,
                            pos: native_pos.or(p.smoothed.map(|(q, _)| q)).unwrap_or(r.pos),
                            name: p.name.clone(),
                            ping_ms: p.rtt_ms,
                        })
                    })
                    .collect()
            };
            // Party HP bars (v7.10): one row per connected peer, in id order. A peer that is
            // loading / hidden keeps its last HP, greyed ("away").
            let mut party = Vec::with_capacity(self.peers.len());
            for p in self.peers.iter() {
                if let Some(r) = p.snapshot.filter(|r| !r.hidden) {
                    self.party_last_hp.insert(p.id, (r.hp, r.max_hp));
                }
                let (hp, max_hp) = self.party_last_hp.get(&p.id).copied().unwrap_or((0, 0));
                party.push(ui::PartyMember { id: p.id, name: p.name.clone(), hp, max_hp, state: party_state(p.snapshot.as_ref(), p.cutscene, here) });
            }
            self.party_last_hp.retain(|id, _| self.peers.by_id(*id).is_some());
            self.party = party;
        } else {
            // Link down: release every host-driven enemy (else they stay
            // pinned, or stand on their NetAI side without records). The
            // world is still loaded here, so their own AI and vtable go back.
            self.enemies.release_all();
            self.friend_tags.clear();
            self.party.clear();
            self.party_last_hp.clear();
        }
        ui::publish_tags(self.friend_tags.clone());
        if hands_off {
            self.boss.hands_off();
            return;
        }

        if cmd.go_to_friend {
            self.join.go_to_friend();
        }
        // Boss rules: announcements, the death-screen wait, the friend's-rest expulsion,
        // rewards. An expulsion starts an idol warp: let go like after a join warp.
        crash::breadcrumb("boss rules");
        let boss_warped = self.boss.on_frame(bossrules::Frame {
            net: self.net.as_mut().and_then(Option::as_mut),
            world: &mut self.world,
            peers: &self.peers,
            player,
            here,
            in_world_for: in_world_since.elapsed(),
            wake: cmd.wake,
        });
        if boss_warped {
            self.after_warp();
            return;
        }
        // Own world (ownworld.rs): a joiner whose session ended gets their own world back
        // (+ the session's progress) through an idol reload: let go like after a join warp.
        crash::breadcrumb("own world");
        if crate::ownworld::on_frame(player, self.probe.current(player), &self.peers, in_world_since.elapsed()) {
            self.after_warp();
            return;
        }
        // Leaving a companion's world: reload at our idol (leave.rs, reload_on_leave), and
        // the host's "a companion entered your world" notice.
        if crate::leave::on_frame(in_world_since.elapsed(), &self.peers) {
            self.after_warp();
            return;
        }
        if cmd.mark {
            static MARKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = MARKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
            log::line(&format!("MARK {n} ms={ms}"));
        }
        // Native remote player: the friend (N1). The N0 test clone had a menu button only.
        crash::breadcrumb("native remote");
        let connected = matches!(self.net.as_ref(), Some(Some(n)) if n.connected());
        // N0 one-machine multi-player test ([DEBUG] n0_clones, PLAN-MULTIPLAYER §3): k local
        // loopback clones after every load (native.rs creates them 3 s apart), after the
        // same settle time as the friend's remote. Back to 0 deletes them.
        let n0 = crate::tuning::n0_clones();
        if n0 == 0 {
            if self.native.has_test() || self.native.n0_requested() {
                self.native.remove_tests("n0_clones = 0");
            }
        } else if !self.native.n0_requested()
            && in_world_since.elapsed().as_secs_f32() > crate::tuning::native_spawn_delay_s() as f32
        {
            self.native.spawn_test(n0);
        }
        if self.native.test_count() > 0 && self.n0_log_at.is_none_or(|t| t.elapsed().as_secs() >= 10) {
            self.n0_log_at = Some(std::time::Instant::now());
            log::line(&format!("native: N0 {} clones, {:.0} fps", self.native.test_count(), self.fps));
        }
        // One native remote per peer in our block (native.rs `wants_for`: v7.5's friend rule
        // per peer, created at most one per 3 s; not while the area still streams in,
        // native_spawn_delay_s: driver crash 1-7 s after a creation right after a load).
        let friend_id = self.peers.first().map(|p| p.id);
        let wants = self.native.wants_for(&self.peers, here, in_world_since.elapsed(), connected);
        self.native.ensure_peers(player, &wants);
        self.native.tick(player, in_world_since.elapsed());
        let native_chr = friend_id.and_then(|id| self.native.remote_of(id));
        // The set's native remotes as they are now (created / removed just above).
        self.refresh_peer_natives();
        // The friend's combat art / prosthetic bullets on the native remote (artfx.rs).
        crate::artfx::tick(Some(player), &self.peers);
        // Experimental native session (nsession.rs): does nothing while native_session = 0.
        if let Some(n) = self.net.as_ref().and_then(Option::as_ref) {
            crate::nsession::tick(&crate::nsession::Frame {
                player,
                peers: &self.peers,
                connected: n.connected(),
                is_host: n.is_host(),
                in_world_for: in_world_since.elapsed(),
                here,
            });
        }
        // The ghost is the fallback for at most one peer, the friend (`peers.first()`, whose
        // snapshots `Net::remote()` reads): only while no native remote stands for it and
        // the native path is off or failing for it.
        let ghost_allowed = native_chr.is_none()
            && (!crate::tuning::native_remote()
                || friend_id.is_some_and(|id| self.native.failing_for(id))
                || self.native.has_test());
        if native_chr.is_some()
            && let Some(g) = self.spawned.filter(|&c| is_ghost_ins(c))
        {
            hook::clear_puppet();
            ghost::despawn(g);
            self.spawned = None;
            self.ghost_died = false;
            log::line("ghost: native remote took over, ghost removed");
        }

        // Mirror the friend's animation on the ghost whenever it changes.
        if let (Some(r), Some(g)) = (remote, self.spawned)
            && crate::tuning::ghost_wevent()
            && r.anim > 0
            && r.anim != self.last_anim
        {
            // Ok(true) = played; Ok(false) = no W_Event for this id (most
            // locomotion: needs the replay-style path B later).
            let result = anim::play_wevent(g, r.anim).or_else(|_| anim::play(g, r.anim).map(|_| true));
            self.last_anim = r.anim;
            self.anims_played += 1;
            if result == Ok(true) {
                self.anims_ok += 1;
            }
            if self.anims_played <= 20 || self.anims_played % 200 == 0 {
                log::line(&format!(
                    "anim: ghost plays {} ({result:?}), {} of {} played so far",
                    r.anim, self.anims_ok, self.anims_played
                ));
            }
        }

        // Client: come to the host (teleport, or idol warp + teleport) on connect. Join
        // follows the host (PLAN-MULTIPLAYER P4), never another client: the host's frame;
        // the compat snapshot only without one (host side, or the v7.5 link-down window).
        let host_frame = self.peers.host();
        let join_target = match host_frame {
            Some(h) => h.snapshot,
            None => remote,
        };
        if let (Some(r), Some(Some(n))) = (join_target, self.net.as_ref())
            && in_world_since.elapsed().as_secs_f32() > 1.0
        {
            crash::breadcrumb("join");
            // The join warp waits for the host's flag dump (world.rs), so the
            // reload builds the map from the host's world state.
            let hold = self.world.join_hold(n.epoch());
            let host_loaded = host_frame.map_or_else(|| n.friend_loaded_recently(10), |h| h.loaded_recently(10));
            let warped = self.join.on_frame(player, n.epoch(), &r, n.is_client(), host_loaded, in_world_since.elapsed(), hold);
            if let Some(idol) = self.join.take_rest_request() {
                self.world.silent_rest(idol);
            }
            if warped {
                self.after_warp();
                return;
            }
        }

        // Friend gone for 3 s (quit, crash, lost link): remove the ghost.
        // "Seen" = the session is alive, not "visible": the friend sends hidden
        // snapshots on every load/death/warp and must not lose the ghost then.
        if matches!(self.net.as_ref(), Some(Some(n)) if n.connected()) {
            self.remote_seen_at = Some(std::time::Instant::now());
        }
        // Only after a friend was seen, so a manual F6 ghost stays.
        let friend_gone = self.remote_seen_at.is_some_and(|t| t.elapsed().as_secs() >= 3);
        if friend_gone {
            self.remote_seen_at = None;
            if let Some(g) = self.spawned.take() {
                hook::clear_puppet();
                ghost::despawn(g);
            }
            self.ghost_died = false;
        }

        // Replay-style animation: friend's states + movement, in step with
        // the delayed position.
        if crate::tuning::ghost_motion()
            && let Some(g) = self.spawned.filter(|c| rtti::class_name(c.0).is_some_and(|n| n.contains("ReplayGhostIns")))
            && let Some(s) = self.net.as_ref().and_then(|n| n.as_ref()).and_then(|n| n.remote_delayed(crate::tuning::ghost_delay_ms()))
        {
            crash::breadcrumb("ghost motion");
            self.motion.apply(g, &s);
        }

        // Once, 3 s after a ghost appears (its model exists by then).
        if let (Some(g), Some(t)) = (self.spawned, self.phantom_log_at)
            && t.elapsed().as_secs() >= 3
        {
            self.phantom_log_at = None;
            ghost::log_phantom_rows(g, player);
        }
        // Solid look: keep the ghost shader value cleared.
        if let Some(g) = self.spawned
            && crate::tuning::ghost_solid() >= 1
        {
            ghost::solid_shader_tick(g);
        }

        // Mirror the friend's HP: 0 kills the ghost; when the friend is back on
        // their feet a dead ghost is replaced by a fresh one.
        if let (Some(r), Some(g)) = (remote, self.spawned) {
            let ghost_hp = g.hp().unwrap_or(0);
            if r.hp > 0 && ghost_hp <= 0 && self.ghost_died {
                hook::clear_puppet();
                ghost::despawn(g);
                self.spawned = None;
                self.ghost_died = false;
                log::line("ghost: friend revived, replacing the dead ghost");
            } else if r.hp <= 0 && ghost_hp > 0 && !self.ghost_died {
                g.set_hp(0);
                self.ghost_died = true;
                log::line("ghost: friend died, ghost dies too");
            }
        }

        // Seamless: while a friend is connected keep exactly one ghost alive,
        // re-created 6 s after every load / death / warp. No key needed.
        let ghost_alive = self.spawned.is_some_and(is_ghost_ins);
        let retry_ok = self.auto_ghost_try.is_none_or(|t| t.elapsed().as_secs() >= 5);
        // 6 s after a load, not 2: the friend crashed in the GPU driver 3 s after a
        // ghost was created right after a warp (2026-10-22), likely while the
        // area was still streaming in. Only for a living friend in our block:
        // a ghost for a friend elsewhere just stood frozen where it spawned.
        let friend_here = remote.is_some_and(|r| r.hp > 0 && here == Some(r.block));
        // Now native_spawn_delay_s (default 10): the same driver crash (gpuguard.rs).
        if friend_here
            && ghost_allowed
            && !ghost_alive
            && retry_ok
            && in_world_since.elapsed().as_secs_f32() > crate::tuning::native_spawn_delay_s() as f32
        {
            crash::breadcrumb("auto ghost");
            self.auto_ghost_try = Some(std::time::Instant::now());
            match ghost::spawn_plain() {
                Ok(g) => {
                    // Solid Wolf instead of the phantom look (before the model
                    // is built, so right after the factory).
                    let solid = crate::tuning::ghost_solid();
                    if solid >= 1
                        && let Err(e) = ghost::make_solid(g)
                    {
                        log::line(&format!("ghost: solid look failed: {e}"));
                    }
                    let _ = ghost::dress_like(g, player);
                    if solid >= 2 {
                        ghost::set_other_player_type(g, player);
                    }
                    // set_look(g, 0) is off: in the test after it was added
                    // the client no longer saw the host's ghost (2026-10-22).
                    ghost::set_translucent(g, self.translucent);
                    self.spawned = Some(g);
                    log::line("ghost: auto-spawned for the connected friend");
                    self.phantom_log_at = Some(std::time::Instant::now());
                    if self.modules_logged < 2 {
                        self.modules_logged += 1;
                        anim::log_modules("ghost", g);
                        anim::log_modules("player", player);
                    }
                }
                Err(e) => log::line(&format!("ghost: auto-spawn failed: {e}")),
            }
        }

        let hits = hook::null_guard_hits();
        let due = self.null_guard_log_at.is_none_or(|t| t.elapsed().as_secs() >= 10);
        if hits != self.null_guard_logged && due {
            self.null_guard_logged = hits;
            self.null_guard_log_at = Some(std::time::Instant::now());
            log::line(&format!("hook: null guard lent the player equip struct {hits} times so far"));
        }

        if f11 {
            crash::breadcrumb("F11 creator dump");
            log::line("puppet: F11 CSDebugChrCreator dump");
            rtti::dump_debug_chr_creator();
        }

        if f10 {
            crash::breadcrumb("F10 exe dump");
            dump::dump_image();
        }
        // F12 = Wolf (c0000, needs the null guard), F5 = Ashina soldier (c1010).
        let spawn_req = if f12 { Some(("c0000", 0u32)) } else if f5 { Some(("c1010", 1010u32)) } else { None };
        if let Some((name, chr_id)) = spawn_req {
            // Only the EnemyIns c0000 path needs the guard; installing it at
            // startup cost FPS for nothing.
            if name == "c0000" {
                hook::install_null_guard();
            }
            crash::breadcrumb("debug spawn request");
            match player.position() {
                Some(p) => match rtti::request_spawn(name, chr_id, [p[0] + 3.0, p[1], p[2]]) {
                    Ok(l) => {
                        log::line(&format!("spawn: {name} (id {chr_id}) request written, watching"));
                        self.spawn_watch = Some((l, 0));
                    }
                    Err(e) => log::line(&format!("spawn: {name} NOT attempted: {e}")),
                },
                None => log::line("spawn: F12 player position unreadable"),
            }
        }
        if let Some((l, frame)) = &mut self.spawn_watch {
            crash::breadcrumb("F12 spawn watch");
            *frame += 1;
            if [1, 2, 3, 5, 10, 20, 30, 60, 120, 180].contains(frame)
                && let Some(p) = rtti::watch_spawn(l, *frame)
            {
                let chr = Chr(p);
                log::line(&format!(
                    "spawn: CREATED ChrIns {p:#x} chr id {:?} entity {:?} pos {:?} hp {:?}",
                    chr.character_id(),
                    chr.entity_id(),
                    chr.position(),
                    chr.hp()
                ));
                // The new character becomes the puppet right away.
                chr.make_puppet();
                self.spawned = Some(chr);
                self.follow = true;
                self.follow_frames = 0;
                self.spawn_watch = None;
            } else if *frame >= 180 {
                log::line("spawn: nothing created within 180 frames");
                self.spawn_watch = None;
            }
        }

        if f7 && self.scan_cursor.is_none() {
            log::line("puppet: F7 scan started");
            self.found.clear();
            self.scan_cursor = Some(SCAN_FIRST);
        }
        if let Some(cursor) = self.scan_cursor {
            crash::breadcrumb("F7 entity scan");
            self.scan_step(cursor, player);
        }

        // F6 / F9: player-model ghost (ReplayGhostIns) as the puppet, two type values.
        // F6 = data-less ghost (stays alive); F9 = replay-data ghost type 3 (was
        // deleted next frame, kept for comparison).
        let ghost_req = if f6 { Some(0u32) } else if f9 { Some(3u32) } else { None };
        if let Some(t) = ghost_req {
            crash::breadcrumb("ghost spawn");
            // Reuse one ghost: each F6 used to add another until the ghost set was
            // full (12, 2026-10-22).
            let existing = self.spawned.filter(|c| rtti::class_name(c.0).is_some_and(|n| n.contains("ReplayGhostIns")));
            let result = match existing {
                Some(c) if t == 0 => Ok(c),
                _ if t == 0 => ghost::spawn_plain(),
                _ => ghost::spawn(t),
            };
            let result = result.inspect(|g| {
                if let Err(e) = ghost::dress_like(*g, player) {
                    log::line(&format!("ghost: dress failed: {e}"));
                }
                ghost::set_translucent(*g, self.translucent);
            });
            match result {
                Ok(chr) => {
                    self.spawned = Some(chr);
                    self.follow = true;
                    self.follow_frames = 0;
                }
                Err(e) => log::line(&format!("ghost: type {t} failed: {e}")),
            }
        }

        if f8 {
            self.follow = !self.follow;
            self.follow_frames = 0;
            if !self.follow {
                hook::clear_puppet();
            }
            log::line(&format!(
                "puppet: F8 follow = {} (hook installed = {})",
                self.follow,
                hook::installed()
            ));
        }
        // Remote snapshots only drive an existing puppet; no per-frame logging
        // before F7 (2026-10-22: 871 lines/minute cost FPS).
        let has_puppet = self.puppet_entity.is_some() || self.spawned.is_some();
        if self.follow || (remote.is_some() && has_puppet) {
            crash::breadcrumb("F8 follow step");
            // Different map block: coordinates are not comparable. Leave the ghost
            // where it is (forcing it 500 m under the map made Havok fight it every
            // frame: 16 FPS for 100 s, 2026-10-22).
            let other_block = remote.is_some_and(|s| here.is_some_and(|b| b != s.block));
            if other_block {
                hook::clear_puppet();
            } else {
                self.follow_step(player, smoothed.map(|(p, _)| p).or(remote.map(|s| s.pos)));
                if let (Some((_, angle)), Some(g)) = (smoothed, self.spawned)
                    && crate::tuning::ghost_angle()
                    && game::sane_angle(angle)
                    && let Some(ph) = g.physics()
                {
                    game::write(ph + 0x74, angle);
                }
            }
        }
    }

    fn scan_step(&mut self, cursor: i32, player: Chr) {
        let end = (cursor + SCAN_PER_FRAME).min(SCAN_LAST);
        for id in cursor..end {
            if let Some(chr) = game::chr_by_entity_id(id)
                && chr != player
                && !self.found.iter().any(|(_, c)| *c == chr)
            {
                self.found.push((id, chr));
            }
        }
        if end < SCAN_LAST {
            if (end - SCAN_FIRST) % 200_000 == 0 {
                log::line(&format!("puppet: scanned up to {end}, found {}", self.found.len()));
            }
            self.scan_cursor = Some(end);
            return;
        }
        self.scan_cursor = None;
        self.finish_scan(player);
    }

    fn finish_scan(&mut self, player: Chr) {
        let Some(ppos) = player.position() else {
            log::line("puppet: player position unreadable");
            return;
        };
        log::line(&format!(
            "puppet: scan done, {} characters; player pos {:?} angle {:?}",
            self.found.len(),
            ppos,
            player.angle()
        ));

        let mut near: Vec<(f32, i32, Chr)> = self
            .found
            .iter()
            .filter_map(|&(id, chr)| Some((game::distance(ppos, chr.position()?), id, chr)))
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (dist, id, chr) in near.iter().take(10) {
            log::line(&format!(
                "puppet:   entity {id} chr c{:04} hp {:?} dist {dist:.1} m",
                chr.character_id().unwrap_or(-1),
                chr.hp()
            ));
        }

        // Skip event dummies: test 2026-10-22 picked entity 1100951 at 0.7 m with
        // hp 9999 next to an idol, which was invisible.
        let pick = near.iter().find(|(dist, _, chr)| {
            let hp = chr.hp().unwrap_or(0);
            *dist >= MIN_PUPPET_DISTANCE && *dist <= MAX_PUPPET_DISTANCE && hp > 0 && hp < 9999
        });
        match pick {
            Some(&(dist, id, chr)) => {
                let ok = chr.make_puppet();
                self.puppet_entity = Some(id);
                log::line(&format!(
                    "puppet: PUPPET = entity {id} (c{:04}, {dist:.1} m), harmless flags ok = {ok}. Press F8 to make it follow.",
                    chr.character_id().unwrap_or(-1)
                ));
            }
            None => log::line(&format!(
                "puppet: no living character within {MAX_PUPPET_DISTANCE} m; walk near an enemy and press F7 again"
            )),
        }
    }

    /// Moves the puppet to the remote player (when connected) or 2 m beside us.
    fn follow_step(&mut self, player: Chr, remote_pos: Option<[f32; 3]>) {
        // A debug-spawned character (F5/F12) wins over an F7 pick. It is only
        // trusted while its vftable still names an EnemyIns.
        let spawned = self.spawned.filter(|c| rtti::class_name(c.0).is_some_and(|n| n.contains("EnemyIns") || n.contains("ReplayGhostIns")));
        if self.spawned.is_some() && spawned.is_none() {
            log::line("spawn: spawned puppet no longer valid");
            self.spawned = None;
        }
        let resolved = match spawned {
            Some(c) => Some(c),
            None => self.puppet_entity.and_then(game::chr_by_entity_id),
        };
        // Re-resolve every frame: the pointer changes after loads/respawns.
        let Some(id) = self.puppet_entity.or(spawned.map(|_| -1)) else {
            log::line("puppet: no puppet yet, press F7 first");
            hook::clear_puppet();
            self.follow = false;
            return;
        };
        let Some(chr) = resolved else {
            log::line(&format!("puppet: entity {id} no longer loaded, follow off"));
            hook::clear_puppet();
            self.follow = false;
            return;
        };
        let Some(pos) = player.position() else {
            return;
        };
        let target = remote_pos.unwrap_or([pos[0] + FOLLOW_OFFSET_X, pos[1], pos[2]]);
        // Hook only: a direct write is overwritten later in the frame, and the
        // 2026-10-22 F10 A/B test showed it adds nothing (puppet still followed).
        let ok = match (chr.physics(), chr.position4()) {
            (Some(physics), Some(cur)) => {
                hook::set_puppet(physics, [target[0], target[1], target[2], cur[3]]);
                true
            }
            _ => false,
        };

        self.follow_frames += 1;
        if self.follow_frames == 1 || self.follow_frames % 300 == 0 {
            log::line(&format!(
                "puppet: follow frame {} hook target set = {ok}, puppet now at {:?} (target {:?})",
                self.follow_frames,
                chr.position(),
                target
            ));
        }
    }
}

impl Puppet {
    #[cfg(feature = "testsync")]
    fn testsync_notice(&self) -> Option<String> {
        self.testsync.notice()
    }

    #[cfg(not(feature = "testsync"))]
    fn testsync_notice(&self) -> Option<String> {
        None
    }
}

// --- The per-frame PeerSet (P1b, PLAN-MULTIPLAYER §5.3) ------------------------------------

impl Puppet {
    /// This frame's other players. For modules owned elsewhere until their P1c switch.
    #[allow(dead_code)] // P1c consumers
    pub fn peers(&self) -> &PeerSet {
        &self.peers
    }

    /// After native.rs `ensure_peers` / `tick` created or removed a remote this frame: the
    /// set's `native` follows (`remote_of`), so the modules after it never see a deleted
    /// chr. `view` stays as built (the native position at the frame's start).
    fn refresh_peer_natives(&mut self) {
        let stale: Vec<PeerFrame> = self
            .peers
            .iter()
            .filter(|p| p.native != self.native.remote_of(p.id))
            .cloned()
            .collect();
        for mut p in stale {
            p.native = self.native.remote_of(p.id);
            self.peers.push(p);
        }
    }

    /// Rebuilds `self.peers` from the link (net.rs), each peer's native remote
    /// (native.rs `remote_of`, none on loading screens: `in_world` false) and world.rs'
    /// committed death. Called once per frame, after `Net::on_frame`, before any module runs.
    fn build_peers(&mut self, in_world: bool) {
        let net = self.net.as_ref().and_then(Option::as_ref);
        let mut set = match net {
            None => PeerSet::solo(),
            Some(n) => PeerSet::new(n.me(), n.is_host()),
        };
        if let Some(n) = net.filter(|n| n.connected()) {
            for p in n.peers() {
                let smoothed = p.remote_smoothed(crate::tuning::ghost_delay_ms());
                // No native remote on loading screens (the world is gone).
                let native = if in_world { self.native.remote_of(p.id()) } else { None };
                // The same view bossrules gets today (`friend_view`).
                let view = native
                    .and_then(|c| c.position().zip(c.angle()))
                    .filter(|(p, _)| game::sane_pos(*p))
                    .or(smoothed);
                set.push(PeerFrame {
                    id: p.id(),
                    name: if p.name().is_empty() { "Friend".to_string() } else { p.name().to_string() },
                    epoch: p.epoch(),
                    rtt_ms: p.rtt_ms(),
                    snapshot: p.remote(),
                    smoothed,
                    native,
                    view,
                    loaded_at: p.loading_at(),
                    dead_since: p.dead_since(),
                    death_at: self.world.peer_death_at(p.id()),
                    cutscene: p.in_cutscene(),
                });
            }
        }
        let ids: Vec<PeerId> = set.iter().map(|p| p.id).collect();
        if ids != self.peers_logged {
            log::line(&format!(
                "peers: me {} ({}), {} player(s): {:?}",
                set.me().slot(),
                if set.is_host() { "host" } else { "not host" },
                set.players(),
                set.iter().map(|p| (p.id.slot(), p.name.as_str())).collect::<Vec<_>>()
            ));
            self.peers_logged = ids;
        }
        crate::peers::SESSION_PLAYERS.store(set.players().min(255) as u8, std::sync::atomic::Ordering::Relaxed);
        self.peers = set;
    }
}

/// After `World::on_frame`: each peer's `death_at` as world.rs has it now (the set was
/// built before the frame's events were taken).
fn refresh_peer_deaths(peers: &mut PeerSet, world: &world::World) {
    let stale: Vec<PeerFrame> = peers.iter().filter(|p| p.death_at != world.peer_death_at(p.id)).cloned().collect();
    for mut p in stale {
        p.death_at = world.peer_death_at(p.id);
        peers.push(p);
    }
}

/// Seamless death rule (RE-DEATH-RESET.md P3): one player dying must not
/// reset the world. A client already follows the host's HP after its own
/// death reload. When the HOST dies and a client lives, the host's reload
/// brings every enemy back, so a few seconds after it the host asks the
/// clients (`EV_BACK` to all) which enemies are dead there; every client sends
/// its list to the host, which kills the union. Every connected player dead
/// within the window (a wipe) = no restore: everyone gets the fresh world
/// (PLAN-MULTIPLAYER §2 death_sync; two players: exactly the v7.5 rule).
fn death_sync(
    world: &mut crate::world::World,
    enemies: &crate::enemies::Enemies,
    n: &mut crate::net::Net,
    peers: &PeerSet,
    player: game::Chr,
    in_world_since: std::time::Instant,
) {
    let wipe_window = std::time::Duration::from_secs(90);
    // Death = automatic rest for the player only (no idol mailboxes: enemies stay as they are).
    // Co-op only (this runs while connected); before the host branch clears own_death_at.
    world.death_refill(in_world_since, crate::tuning::death_restore_delay_s() as u64);
    if n.is_host() {
        if let Some(t) = world.own_death_at
            // Back in the world after the death reload, settled: death_restore_delay_s
            // (default 8, was 4; ForceDead on 6 enemies 1 s before a driver crash).
            && in_world_since > t
            && in_world_since.elapsed().as_secs() >= crate::tuning::death_restore_delay_s() as u64
        {
            world.own_death_at = None;
            // A rest of any player after our death reset the world for all
            // (restsync.rs): the clients' dead from before it must not come back.
            if crate::restsync::rest_after(t) {
                log::line("death: a rest since our death reset the world for everyone: no restore");
                return;
            }
            let dead: Vec<bool> = peers
                .iter()
                .map(|p| {
                    peer_dead_for_wipe(
                        world.peer_death_at(p.id).map(|f| f.elapsed()),
                        // It died in a boss fight and waits on its death screen for us.
                        world.peer_down(p.id),
                        wipe_window,
                    )
                })
                .collect();
            if all_dead(&dead) {
                // No boss record from anybody may come back after a wipe (live 2026-10-24: the
                // friend's old Genichiro record was restored 2 s after "no restore").
                crate::persist::note_wipe_loaded("death_sync");
                log::line(&format!(
                    "death: {} died (wipe): no restore, fresh world for everyone",
                    if dead.len() > 1 { "every player" } else { "both" }
                ));
            } else {
                n.send_event(crate::world::EV_BACK, 0, 0);
                crate::restsync::mark_back_sent();
                log::line(&format!(
                    "death: back after our death ({:.1} s after the load, death_restore_delay_s {}), asking {} which enemies are dead",
                    in_world_since.elapsed().as_secs_f32(),
                    crate::tuning::death_restore_delay_s(),
                    if dead.len() > 1 { "every client" } else { "the friend" }
                ));
            }
        }
        // Every client's list: the union is killed here.
        while let Some((from, d)) = n.take_bulk_from(crate::world::BULK_DEAD_LIST) {
            let ids: Vec<i32> = d.chunks_exact(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
            if crate::restsync::rest_since_back() {
                log::line(&format!(
                    "death: peer {}'s list of {} dead enemies is from before a rest: ignored (a rest resets for everyone)",
                    from.slot(),
                    ids.len()
                ));
                continue;
            }
            if crate::persist::wipe_recent(crate::persist::WIPE_STALE) {
                log::line(&format!(
                    "death: peer {}'s list of {} dead enemies ignored: wipe recent (stale before the wipe)",
                    from.slot(),
                    ids.len()
                ));
                continue;
            }
            let (killed, other) = enemies.kill_ids(&ids);
            log::line(&format!(
                "death: peer {} reported {} dead enemies: killed {killed} here, {other} already dead or not loaded",
                from.slot(),
                ids.len()
            ));
        }
    } else if world.take_peer_back(PeerId::HOST) {
        let host_death_at = world.peer_death_at(PeerId::HOST);
        let we_died = world.own_death_at.is_some_and(|t| t.elapsed() < wipe_window);
        if we_died || player.hp().is_none_or(|hp| hp <= 0) {
            log::line("death: host is back but we died too (wipe): nothing to report");
        } else if host_death_at.is_some_and(crate::restsync::rest_after) {
            // A rest after the host's death reset our enemies too (restsync.rs).
            log::line("death: host is back, but a rest since its death reset the world for everyone: nothing to report");
        } else {
            let ids = enemies.dead_here_ids();
            let bytes: Vec<u8> = ids.iter().flat_map(|id| id.to_le_bytes()).collect();
            // To the host only (the other clients have no use for it).
            let ok = n.send_bulk_to(crate::net::Dest::Host, crate::world::BULK_DEAD_LIST, &bytes);
            log::line(&format!("death: host is back, reported {} enemies dead here (ok = {ok})", ids.len()));
        }
    }
}

/// One peer counts as dead for the host's wipe check: its committed death within the
/// window (`since` = time since it), or down on its death screen in a boss fight. HP 0 alone
/// is not death: in Sekiro the Resurrect choice waits without limit.
fn peer_dead_for_wipe(since: Option<std::time::Duration>, down: bool, window: std::time::Duration) -> bool {
    since.is_some_and(|s| s < window) || down
}

/// A wipe: there is someone and everyone is dead (no peer: never, as before).
fn all_dead(dead: &[bool]) -> bool {
    !dead.is_empty() && dead.iter().all(|&d| d)
}

/// Max HP and posture lost per death stack (ER Seamless uses about 20 %).
const DEBUFF_PCT: u32 = 10;
/// Bulk kind of the host's scaling percentages (world.rs uses 1, 5 and 6).
const BULK_SCALE_CFG: u8 = 7;

fn own_scale_cfg(players: u32) -> crate::scaling::ScaleCfg {
    use std::sync::atomic::Ordering::Relaxed;
    let p = |a: &std::sync::atomic::AtomicU32| a.load(Relaxed).min(1000) as u16;
    crate::scaling::ScaleCfg {
        enemy_hp: p(&crate::tuning::ENEMY_HEALTH_SCALING),
        enemy_posture: p(&crate::tuning::ENEMY_POSTURE_SCALING),
        boss_hp: p(&crate::tuning::BOSS_HEALTH_SCALING),
        boss_posture: p(&crate::tuning::BOSS_POSTURE_SCALING),
        players: players.clamp(1, crate::peers::MAX_PLAYERS as u32) as u8,
        debuffs: Some(crate::tuning::DEATH_DEBUFFS.load(Relaxed)),
    }
}

/// The five settings the 'Mod settings' panel edits, as this machine has them now.
fn menu_values() -> crate::menu::Values {
    use std::sync::atomic::Ordering::Relaxed;
    let p = |a: &std::sync::atomic::AtomicU32| crate::menu::clamp_pct(a.load(Relaxed) as i64);
    crate::menu::Values {
        enemy_hp: p(&crate::tuning::ENEMY_HEALTH_SCALING),
        enemy_posture: p(&crate::tuning::ENEMY_POSTURE_SCALING),
        boss_hp: p(&crate::tuning::BOSS_HEALTH_SCALING),
        boss_posture: p(&crate::tuning::BOSS_POSTURE_SCALING),
        death_debuffs: crate::tuning::DEATH_DEBUFFS.load(Relaxed),
    }
}

/// Host: the players the enemy scaling counts (`[GAMEPLAY] scaling_count`).
fn scale_players(peers: &PeerSet, here: Option<i32>) -> u32 {
    crate::scaling::players_for(peers.players(), peers.in_block(here).count() as u32, crate::tuning::scaling_count())
}

/// A party HP bar row's state (`ui::PartyMember::state`) for a peer's latest snapshot:
/// 3 = in a cutscene (checked first: its snapshot reads hidden, which would say away),
/// 1 = away (no visible snapshot: loading / hidden / stale, or another map block),
/// 2 = dead (0 HP), 0 = here. `cutscene` = the peer's fresh snapshot says so (hidden ones
/// are not in `snap`); a visible snapshot carrying the bit counts too.
fn party_state(snap: Option<&coop_net::Snapshot>, cutscene: bool, here: Option<i32>) -> u8 {
    if cutscene || snap.is_some_and(|r| r.cutscene) {
        return 3;
    }
    match snap {
        Some(r) if !r.hidden && here == Some(r.block) => {
            if r.hp <= 0 {
                2
            } else {
                0
            }
        }
        _ => 1,
    }
}

#[cfg(test)]
mod party_tests {
    use super::*;

    #[test]
    fn party_row_state_per_peer() {
        let s = |hp: i32, block: i32, hidden: bool| coop_net::Snapshot { hp, block, hidden, ..Default::default() };
        assert_eq!(party_state(Some(&s(500, 1100, false)), false, Some(1100)), 0);
        assert_eq!(party_state(Some(&s(0, 1100, false)), false, Some(1100)), 2);
        assert_eq!(party_state(Some(&s(500, 1200, false)), false, Some(1100)), 1);
        assert_eq!(party_state(Some(&s(500, 1100, true)), false, Some(1100)), 1);
        assert_eq!(party_state(None, false, Some(1100)), 1);
        // In a cutscene (hidden snapshot, none visible, another block, even dead): 3, before away.
        let c = coop_net::Snapshot { cutscene: true, ..s(500, 1100, true) };
        assert_eq!(party_state(Some(&c), false, Some(1100)), 3);
        assert_eq!(party_state(None, true, Some(1100)), 3);
        assert_eq!(party_state(Some(&s(500, 1200, false)), true, Some(1100)), 3);
        assert_eq!(party_state(Some(&s(0, 1100, false)), true, Some(1100)), 3);
    }
}

#[cfg(test)]
mod death_sync_tests {
    use super::*;
    use std::time::Duration;

    /// P4 death_sync: the host's death is a wipe only when every connected peer is dead
    /// (3-6 players); one living client gets the dead-list request. Two players: v7.5.
    #[test]
    fn host_death_wipe_needs_every_peer_dead() {
        let w = Duration::from_secs(90);
        assert!(peer_dead_for_wipe(Some(Duration::from_secs(10)), false, w), "committed death");
        assert!(!peer_dead_for_wipe(Some(Duration::from_secs(91)), false, w), "too long ago");
        assert!(peer_dead_for_wipe(None, true, w), "down on its death screen");
        assert!(!peer_dead_for_wipe(None, false, w), "at 0 HP on the resurrect screen, hidden: alive");
        assert!(!all_dead(&[]), "no peer: never a wipe");
        assert!(all_dead(&[true]));
        assert!(!all_dead(&[false]));
        for n in 2..=5 {
            let mut d = vec![true; n];
            assert!(all_dead(&d), "{} players", n + 1);
            d[n / 2] = false;
            assert!(!all_dead(&d), "one of {} alive: restore", n + 1);
        }
    }
}
