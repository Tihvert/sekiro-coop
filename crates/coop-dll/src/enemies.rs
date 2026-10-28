//! Host-authoritative enemy sync (first version, 2026-10-22).
//!
//! Host: finds enemies by entity id (same ids in both games: same map), and
//! 20 times a second sends the ones near either player: position, angle, HP,
//! current animation. Applies damage the client reports (cumulative totals,
//! so a lost packet loses nothing).
//! Client: pulls the same enemies to the host's positions through the
//! UpdateCoords hook (table entries 1..), smoothing between updates; copies
//! HP (so deaths match) and animation; any HP the enemy lost locally since our
//! last copy is the client's damage and is reported to the host.
//! Enemy AI stays on in the client's game, so enemies can still hit the client.
//!
//! Mode 4 (NetAI, 2026-10-22): the host sends the raw records its AI writes
//! into the WorldChrMan sync table (netai.rs) at 25 Hz instead; the client
//! switches each enemy to its NetAIManipulator, which moves and animates it
//! from those records like a DS3 co-op guest would. HP/death authority and
//! the client's damage reports stay exactly as above. An enemy the client
//! cannot switch falls back to mode-2 positions through the hook.
//!
//! N players (PLAN-MULTIPLAYER §5.15, mode4.rs): the host stays the only authority and
//! sends states / records / posture to All; the sync radius covers every peer; each
//! client reports its hits to the host only, and the host keeps the totals per
//! (client, enemy); NetAI player targets are `0x10 + slot`; session state restarts per
//! peer link. Clients take enemy data from the host only. With one peer everything is
//! as in v7.5.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use coop_net::{EnemyState, NETAI_ACTION_LEN, NETAI_BEHAVIOR_LEN, NETAI_MOVE_LEN, NetAiEntry};

use crate::anim::Probe;
use crate::game::{self, Chr};
use crate::hook;
use crate::log;
use crate::mode4::{self, DamageTotals, SessionEpochs};
use crate::net::{Dest, Net};
use crate::peers::{PeerId, PeerSet};
use crate::netai;
use crate::tuning;

const SCAN_FIRST: i32 = 1_000_000;
const SCAN_LAST: i32 = 2_600_000;
// 1500 native lookups per frame: a full sweep takes ~18 s (review: 4000/frame
// cost several ms).
const SCAN_PER_FRAME: i32 = 1_500;
const RESCAN_EVERY: Duration = Duration::from_secs(45);
const RESEND_DAMAGE_EVERY: Duration = Duration::from_millis(250);
const RESEND_DAMAGE_FOR: Duration = Duration::from_secs(3);
const LETHAL_HOLD: Duration = Duration::from_millis(500);
const SEND_EVERY: Duration = Duration::from_millis(50); // 20 Hz
const SYNC_RADIUS_M: f32 = 100.0;
const STALE_AFTER: Duration = Duration::from_millis(1000);
const SNAP_DISTANCE_M: f32 = 8.0;
const ALIVE_FRAMES_FOR_DAMAGE: u32 = 10;
/// Client: after our own hit, the local game owns the enemy's animation and
/// position for a while (parries, deathblows). Test 2026-10-22: the host's
/// generic death animation overrode the client's deathblow, so the kill
/// looked like the enemy "just died".
const OWN_COMBAT_FOR: Duration = Duration::from_millis(2500);

/// NetAI: 25 Hz host ticks (the doc's 20-30 Hz; the NetAIManipulator
/// derives the velocity from the time between records, so a steady rate
/// matters more than a high one).
const NETAI_EVERY: Duration = Duration::from_millis(40);
/// Enemies this close to either player get movement every tick ...
const NETAI_NEAR_M: f32 = 30.0;
/// ... the rest (up to SYNC_RADIUS_M) every 4th (~6 Hz).
const NETAI_FAR_EVERY: u32 = 4;
/// A changed behavior/action record is sent this many times in a row under
/// the same seq (the client applies it once): one lost datagram must not
/// lose an attack, and resending it under a new seq would replay it.
const NETAI_SENDS: u8 = 3;
/// Bytes per tick: 1600 B * 25 Hz = 40 KB/s at most. New behavior/action
/// first, then near movement, repeats, far movement.
const NETAI_TICK_BYTES: usize = 1600;
/// Client: an enemy NetAI refused is tried again after this long.
const NETAI_RETRY: Duration = Duration::from_secs(5);
const STATS_EVERY: Duration = Duration::from_secs(20);

/// Host, per enemy: what was sent, to send records only when they change.
#[derive(Default)]
struct HostAi {
    beh_raw: Vec<u8>,
    beh_wire: Option<Vec<u8>>,
    beh_seq: u8,
    beh_left: u8,
    beh_new: bool,
    act: Vec<u8>,
    act_seq: u8,
    act_left: u8,
    act_new: bool,
    logged: bool,
}

/// Client, per enemy: received records not written yet, and NetAI state.
#[derive(Default)]
struct ClientAi {
    movement: Option<Vec<u8>>,
    behavior: Option<Vec<u8>>,
    beh_seq: Option<u8>,
    action: Option<Vec<u8>>,
    act_seq: Option<u8>,
    seen_move: bool,
    /// The player (session slot: ourselves or a peer) a host-driven enemy turns to:
    /// its position target is refreshed every frame (mode4.rs `player_pos`).
    follow: Option<PeerId>,
    engaged: Option<Chr>,
    refused_at: Option<Instant>,
    refuse_logged: bool,
}

#[derive(Default)]
struct NetAiStats {
    in_range: u32,
    with_net_id: u32,
    with_entry: u32,
    move_reads: u32,
    move_written_seen: u32,
    beh_records: u32,
    act_records: u32,
    unmapped_targets: u32,
    deferred: u32,
    bytes_at: u64,
    // Client.
    engaged_logs: u32,
    active_frames: u32,
    checked_frames: u32,
    inactive_logs: u32,
    moves_written: u32,
    behs_written: u32,
    acts_written: u32,
    behs_bad: u32,
    fallbacks: u32,
    flipped_ai: u32,
    flipped_other: u32,
    enforced: u32,
}

/// Unregisters every host-driven enemy when the enemy state goes away
/// (panic reset in lib.rs, reset() below): our slot 37 then passes the
/// game's own choice through, i.e. the enemies get their AI back.
#[derive(Default)]
struct ForgetOnDrop;

impl Drop for ForgetOnDrop {
    fn drop(&mut self) {
        netai::forget_all();
    }
}

#[derive(Default)]
pub struct Enemies {
    // Both roles: entity-id scan.
    scan_cursor: Option<i32>,
    scanning: Vec<(i32, Chr)>,
    known: Vec<(i32, Chr)>,
    last_scan: Option<Instant>,
    // Host.
    last_send: Option<Instant>,
    /// Host: the damage total applied so far, per reporting peer and enemy (a total
    /// restarts per link and sender).
    applied_damage: DamageTotals,
    damage_logs: u32,
    /// Client: consecutive frames each enemy was seen alive here.
    alive_frames: HashMap<i32, u32>,
    /// Client: last host positions per enemy (receive time), for interpolation.
    history: HashMap<i32, std::collections::VecDeque<(Instant, [f32; 3])>>,
    /// Host: ids sent last update. Client: ids the local game owns now.
    sent_ids: Vec<i32>,
    owned: HashSet<i32>,
    // Client.
    latest: HashMap<i32, (Instant, EnemyState)>,
    slots: HashMap<i32, usize>,
    smooth: HashMap<i32, [f32; 3]>,
    last_written_hp: HashMap<i32, i32>,
    dealt_total: HashMap<i32, i32>,
    last_anim: HashMap<i32, i32>,
    stats_at: Option<Instant>,
    /// Client: when each id was last hit (damage re-sent for a few seconds).
    last_hit: HashMap<i32, Instant>,
    last_resend: Option<Instant>,
    /// Client: when our hit took an enemy to 0 HP (don't revive it meanwhile).
    lethal_at: HashMap<i32, Instant>,
    /// Link epochs per peer (host: every client; client: the host link): a new link
    /// restarts that peer's damage / posture totals, all of them when nobody stayed.
    epochs: SessionEpochs,
    /// Client: enemy states / records / posture from a peer other than the host (dropped).
    not_host_rx: u32,
    // NetAI (mode 4).
    last_mode: u8,
    netai_tick: u32,
    last_netai: Option<Instant>,
    host_ai: HashMap<i32, HostAi>,
    client_ai: HashMap<i32, ClientAi>,
    /// Client: enemies we set CompletelyNoMove on (fallback option).
    no_move: HashSet<i32>,
    netai_stats: NetAiStats,
    netai_stats_at: Option<Instant>,
    /// Client: enemies handed to their own AI for a throw (since, last reason seen).
    handoff: HashMap<i32, (Instant, Instant)>,
    facing: Facing,
    /// Last sync-trace line (both sides, 1 Hz; see `trace`).
    trace_at: Option<Instant>,
    /// Loot for kills that were not ours (loot_step): enemies seen alive this load,
    /// when each was first seen dead, ids already handled, and ids killed by our
    /// death-restore (no rewards for those).
    seen_alive: HashSet<i32>,
    dead_since: HashMap<i32, Instant>,
    looted: HashSet<i32>,
    no_loot: std::cell::RefCell<HashSet<i32>>,
    /// Killed by our ForceDead(noReward = 1) for a kill we were not paid for yet
    /// (livesync / dead set / P3 restore): `loot_step` pays our own drops and sen once,
    /// clearing the "no reward" bit `chr+0x1A12 & 0x10` that ForceDead set (live 7.4:
    /// `loot: 1100800 skipped, no-drop bit set (bits 0x10)`; IDA v75 o1/o9: the drop
    /// `0x9FFF50` and sen/XP `0x9FFD50` both bail out through `0x9F2DE0` on that bit).
    force_loot: std::cell::RefCell<HashSet<i32>>,
    loot_at: Option<Instant>,
    clamp_logs: u32,
    /// Posture / deathblow totals and the host posture mirror (posture.rs).
    posture: crate::posture::Posture,
    /// Client, rest window (restsync.rs): enemies alive here and dead on the host that
    /// were left alive, and how many such lines were logged.
    rest_held: HashSet<i32>,
    rest_logs: u32,
    /// Native session: stealth ownership claims (entity -> netId, last time the approach
    /// held, reason), and whether the enemy was seen on its local AI since the claim.
    claims: HashMap<i32, (u16, Instant, &'static str, bool)>,
    claim_logs: u32,
    /// Last `stealth_claims` run (rate limit, see there).
    claims_at: Option<Instant>,
    _forget: ForgetOnDrop,
}

/// The one gate between the native session and our own sync (mode 2/3/4): true only
/// while nsession is engaged and owns the enemies (`step_aside::enemy_sync_off`, also
/// read by posture.rs `enabled`). The N-player decision lives in nsession (P5, PLAN
/// §5.14 / `own_sync_rule`): more than `native_session_max_players` players, or 3+
/// without a type-23 recipient wrap, or a same-block peer that cannot join, never
/// engage, so this stays false and mode 4 runs for everyone, every frame, with no
/// further switch here. If nsession ever engages only some peers, this is the place to
/// make it per peer (host: keep sending records to the others).
fn native_owns_enemies() -> bool {
    crate::nsession::step_aside::enemy_sync_off()
}

impl Enemies {
    /// Forget everything (area load): every character pointer may be freed,
    /// so nothing is written back (host-driven enemies are only forgotten;
    /// their objects die with the world).
    pub fn reset(&mut self) {
        for (_, slot) in self.slots.drain() {
            hook::clear_target(slot);
        }
        netai::forget_all();
        // Damage accounting is per session, not per load: the epoch must
        // survive too. It used to reset to 0 here, so the next frame saw an
        // "epoch change" and cleared the totals after every load: after the
        // client warped, its new totals restarted at 0 while the host still
        // held the old ones, and the host ignored the client's hits until
        // they added up past the old total (or, the other way round, a host
        // load made the client's re-sent totals land a second time).
        *self = Self {
            applied_damage: std::mem::take(&mut self.applied_damage),
            dealt_total: std::mem::take(&mut self.dealt_total),
            posture: std::mem::take(&mut self.posture).after_load(),
            epochs: std::mem::take(&mut self.epochs),
            last_mode: self.last_mode,
            ..Default::default()
        };
    }

    /// Release every enemy while the world is still loaded (link down, mode
    /// change, sync off): NetAI enemies get their own AI and vtable back,
    /// CompletelyNoMove is cleared, then everything is forgotten.
    pub fn release_all(&mut self) {
        if self.client_ai.values().any(|a| a.engaged.is_some()) || !self.no_move.is_empty() {
            let mut n = 0;
            for (&id, ai) in self.client_ai.iter_mut() {
                if let Some(c) = ai.engaged.take() {
                    netai::release(c, game::chr_by_entity_id(id) == Some(c));
                    n += 1;
                }
            }
            for id in self.no_move.drain() {
                if let Some(c) = game::chr_by_entity_id(id) {
                    netai::set_no_move(c, false);
                }
            }
            log::line(&format!("netai: released {n} host-driven enemies"));
        }
        if !self.slots.is_empty() || !self.latest.is_empty() || !self.client_ai.is_empty() {
            self.reset();
        }
    }

    /// restsync.rs, the frame after a rest of ours: the enemies were respawned in place
    /// (no load), so the per-load kill bookkeeping starts over. Before, `looted` kept
    /// every enemy that had died this load, so a respawned enemy the friend killed again
    /// gave no loot here, and our HP baseline could count the respawn's jump as news.
    pub fn on_rest(&mut self) {
        let looted = self.looted.len();
        self.looted.clear();
        self.dead_since.clear();
        self.no_loot.borrow_mut().clear();
        self.force_loot.borrow_mut().clear();
        self.lethal_at.clear();
        self.alive_frames.clear();
        self.last_written_hp.clear();
        self.rest_held.clear();
        self.rest_logs = 0;
        log::line(&format!("enemies: rest: kill bookkeeping reset ({looted} looted ids forgotten)"));
    }

    /// Host: the enemies sent to the client in the last update.
    pub fn synced(&self) -> Vec<(i32, Chr)> {
        self.known.iter().filter(|(id, _)| self.sent_ids.contains(id)).copied().collect()
    }

    /// Client: the local game currently owns this enemy (we fight it, or it is
    /// dead here): host behavior events must not override it.
    pub fn locally_owned(&self, id: i32) -> bool {
        self.owned.contains(&id)
    }

    /// `peers`: this frame's other players. `ghost`: puppet's chr for "the friend" (the
    /// validated ReplayGhostIns, else the lowest-id native remote), a possible AI target
    /// on the host; every peer's own native remote is one too (`PeerFrame::native`).
    pub fn on_frame(&mut self, net: &mut Net, peers: &PeerSet, player: Chr, ghost: Option<Chr>, probe: &Probe) {
        // The first peer's raw snapshot position (the 2-player friend: the trace line).
        let friend_pos: Option<[f32; 3]> = peers.first().and_then(|p| p.snapshot).map(|s| s.pos);
        let mode = tuning::enemy_mode();
        if mode != self.last_mode {
            // Leaving NetAI (or entering it): hand every enemy back first,
            // the new mode starts from the game's own state.
            if self.last_mode == tuning::ENEMY_NETAI || mode == tuning::ENEMY_NETAI {
                self.release_all();
            }
            self.last_mode = mode;
        }
        if mode == tuning::ENEMY_OFF {
            self.release_all();
            return;
        }
        // A new session restarts damage accounting on both sides; per peer link (host:
        // each client, client: the host link). A second client joining restarts only its
        // own totals (the global `Net::epoch()` changed with every join on the host).
        let joins = self.epochs.update(&peers.link_epochs());
        if joins.fresh {
            self.applied_damage.clear();
            self.dealt_total.clear();
            self.posture.new_session();
        } else {
            for &id in &joins.joined {
                self.applied_damage.forget_peer(id);
                self.posture.forget_peer(id);
                log::line(&format!("enemies: peer {} on a new link: its damage / posture totals start at 0", id.slot()));
            }
        }
        // Experimental native session (nsession.rs `step_aside`): the game's own netcode
        // owns the enemies, so all of the sync below stays off. The scan (scaling, death
        // rule) and the loot step (items stay ours) keep running.
        if native_owns_enemies() {
            self.release_all();
            let _ = net.take_damage_from();
            let _ = net.take_netai_from();
            self.scan_step(player);
            // Stealth / plunging deathblows: ask for ownership of the enemy we sneak up on.
            let others: Vec<[f32; 3]> = peers.iter().filter_map(|p| p.snapshot).filter(|s| s.hp > 0).map(|s| s.pos).collect();
            self.stealth_claims(player, &others);
            self.loot_step(net.is_host());
            self.trace(net, player, friend_pos, ghost);
            return;
        }
        if !self.claims.is_empty() {
            self.claims.clear();
            crate::nsession::set_stealth_claims(Vec::new());
        }
        if net.is_host() {
            self.scan_step(player);
            self.host_damage(net);
            // Every peer's position counts for the sync radius.
            let peer_pos = mode4::peer_positions(peers);
            if mode == tuning::ENEMY_NETAI {
                self.host_netai(net, peers, player, &peer_pos, ghost);
            } else {
                self.host_frame(net, player, &peer_pos, probe);
            }
            if crate::posture::enabled() {
                let synced = self.synced();
                self.posture.host_send(net, &synced, Instant::now());
            }
        } else {
            // The client scans too: after the host's death reload it reports every
            // enemy that is dead here (RE-DEATH-RESET.md P3).
            self.scan_step(player);
            if crate::posture::enabled() {
                let list = self.from_host(net.take_posture_from());
                self.posture.receive(list, Instant::now());
            }
            self.client_frame(net, peers, player);
        }
        self.trace(net, player, friend_pos, ghost);
        self.loot_step(net.is_host());
    }

    fn scan_step(&mut self, player: Chr) {
        if self.scan_cursor.is_none() && self.last_scan.is_none_or(|t| t.elapsed() >= RESCAN_EVERY) {
            self.scan_cursor = Some(SCAN_FIRST);
            self.scanning.clear();
        }
        let Some(cursor) = self.scan_cursor else { return };
        let end = (cursor + SCAN_PER_FRAME).min(SCAN_LAST);
        for id in cursor..end {
            if let Some(c) = game::chr_by_entity_id(id)
                && c != player
                && !self.scanning.iter().any(|(_, k)| *k == c)
            {
                self.scanning.push((id, c));
            }
        }
        if end < SCAN_LAST {
            self.scan_cursor = Some(end);
        } else {
            self.scan_cursor = None;
            self.last_scan = Some(Instant::now());
            self.known = std::mem::take(&mut self.scanning);
            log::line(&format!("enemies: scan found {} characters", self.known.len()));
        }
    }

    /// Host: damage from the clients (every mode), cumulative totals per (client, enemy).
    fn host_damage(&mut self, net: &mut Net) {
        let posture_on = crate::posture::enabled();
        for (from, r) in net.take_damage_from() {
            let (id, total) = (r.id, r.hp);
            // Posture and deathblows first (posture.rs): a deathblow takes the
            // life before the HP loss can reach 0, and one that leaves a life
            // refills the enemy (its HP loss is then not applied).
            let apply_hp = !posture_on || self.posture.host_apply(from, game::chr_by_entity_id(id), &r);
            let delta = self.applied_damage.take(from, id, total);
            if delta <= 0 {
                continue;
            }
            if !apply_hp {
                continue;
            }
            let Some(chr) = game::chr_by_entity_id(id) else { continue };
            let hp = chr.hp().unwrap_or(0);
            let new_hp = (hp - delta).max(0);
            // The game's setter keeps a multi-life enemy at 1 HP (deathblow
            // window) instead of the raw write's 0.
            if !posture_on || !crate::posture::set_hp(chr, new_hp) {
                chr.set_hp(new_hp);
            }
            let got = chr.hp().unwrap_or(new_hp);
            self.damage_logs += 1;
            if self.damage_logs <= 10 || self.damage_logs % 100 == 0 {
                log::line(&format!("enemies: friend {} hit {id} for {delta}: hp {hp} -> {got}", from.slot()));
            }
        }
    }

    fn host_frame(&mut self, net: &mut Net, player: Chr, peer_pos: &[[f32; 3]], probe: &Probe) {
        let now = Instant::now();
        if self.last_send.is_some_and(|t| now.duration_since(t) < SEND_EVERY) {
            return;
        }
        // Fixed step (see net.rs SEND_EVERY): steady 20 Hz, not 15-20.
        self.last_send = Some(match self.last_send {
            Some(t) if now.duration_since(t) < 3 * SEND_EVERY => t + SEND_EVERY,
            _ => now,
        });
        let Some(me) = player.position() else { return };
        let mut states = Vec::new();
        let mut sent_ids = Vec::new();
        for &(id, chr) in &self.known {
            // `known` is up to RESCAN_EVERY old: a character that despawned
            // meanwhile reads as garbage. Never send that to the client.
            let Some(pos) = chr.position().filter(|&p| game::sane_pos(p)) else { continue };
            let near = game::distance(pos, me) <= SYNC_RADIUS_M
                || peer_pos.iter().any(|&f| game::distance(pos, f) <= SYNC_RADIUS_M);
            if !near {
                continue;
            }
            sent_ids.push(id);
            states.push(EnemyState {
                id,
                pos,
                angle: chr.angle().unwrap_or(0.0),
                hp: chr.hp().unwrap_or(0),
                anim: probe.current(chr).unwrap_or(0),
            });
        }
        if !states.is_empty() {
            net.send_enemies(&states);
        }
        self.sent_ids = sent_ids;
        if self.stats_at.is_none_or(|t| t.elapsed().as_secs() >= 20) {
            self.stats_at = Some(Instant::now());
            log::line(&format!("enemies: host sending {} of {} known", states.len(), self.known.len()));
        }
    }

    /// Host, mode 4: the AI's own sync records for every enemy in range of any player,
    /// sent to All (one broadcast; the session delivers it to every client).
    fn host_netai(&mut self, net: &mut Net, peers: &PeerSet, player: Chr, peer_pos: &[[f32; 3]], ghost: Option<Chr>) {
        let now = Instant::now();
        if self.last_netai.is_some_and(|t| now.duration_since(t) < NETAI_EVERY) {
            return;
        }
        self.last_netai = Some(match self.last_netai {
            Some(t) if now.duration_since(t) < 3 * NETAI_EVERY => t + NETAI_EVERY,
            _ => now,
        });
        self.netai_tick = self.netai_tick.wrapping_add(1);
        let tick = self.netai_tick;
        let Some(me) = player.position() else { return };

        // The AI names its target by the host's netId: map it to something
        // the client can resolve (enemy entity id or a player's session slot).
        let by_net: HashMap<u16, i32> =
            self.known.iter().filter_map(|&(id, c)| Some((netai::net_id(c)?, id))).collect();
        // (netId, slot) of our player and every peer's remote, looked up once per tick
        // when a record needs it.
        let mut player_ids: Option<Vec<(u16, PeerId)>> = None;

        // (class, distance, entry, id)
        let mut cands: Vec<(u8, f32, NetAiEntry, i32)> = Vec::new();
        let mut sent_ids = Vec::new();
        let st = &mut self.netai_stats;
        for &(id, chr) in &self.known {
            let Some(pos) = chr.position().filter(|&p| game::sane_pos(p)) else { continue };
            let dist = peer_pos.iter().fold(game::distance(pos, me), |d, &f| d.min(game::distance(pos, f)));
            if dist > SYNC_RADIUS_M {
                continue;
            }
            // Not updated by the game here (far from us): no records to drive the
            // friend's copy with, so it keeps its own AI there (netai.rs).
            if netai::dormant_here(id, chr) {
                continue;
            }
            sent_ids.push(id);
            st.in_range += 1;
            let near = dist <= NETAI_NEAR_M;
            // A re-created entry (retain below, a reset) must not restart its
            // seqs at 1: the client may still hold seq 1 for this id and would
            // drop the first new record as a repeat.
            let ai = self.host_ai.entry(id).or_insert_with(|| HostAi {
                beh_seq: tick as u8,
                act_seq: (tick >> 8) as u8,
                ..Default::default()
            });
            let nid = netai::net_id(chr);
            let ent = nid.and_then(netai::entry);
            st.with_net_id += nid.is_some() as u32;
            st.with_entry += ent.is_some() as u32;
            if !ai.logged {
                ai.logged = true;
                // Live check 1: map enemies have a netId and a NetAIManipulator.
                log::line(&format!(
                    "netai host: enemy {id} netId {:?} netai manip {:?} table entry {}",
                    nid.map(|n| format!("{n:#x}")),
                    netai::netai_manip(chr).map(|p| format!("{p:#x}")),
                    ent.is_some()
                ));
            }

            let mut e = NetAiEntry { id, hp: chr.hp().unwrap_or(0), ..Default::default() };
            if near || tick % NETAI_FAR_EVERY == 0 {
                // Live check 2: the AI rewrites the record every frame (0x02).
                let from_table = ent.filter(|e| netai::flags(e) & netai::F_MOVE_WRITTEN != 0);
                st.move_reads += ent.is_some() as u32;
                st.move_written_seen += from_table.is_some() as u32;
                e.movement = match from_table {
                    Some(t) => {
                        netai::clear_flags(&t, netai::F_MOVE_WRITTEN);
                        netai::read_record(t.movement, NETAI_MOVE_LEN)
                    }
                    None => netai::movement_from_physics(chr),
                };
            }
            if let Some(t) = ent {
                let flags = netai::flags(&t);
                if let Some(raw) = netai::read_record(t.behavior, NETAI_BEHAVIOR_LEN) {
                    let written = flags & netai::F_BEHAVIOR_WRITTEN != 0;
                    // Never written yet (all zero) -> nothing to replay.
                    let valid = raw[netai::BEH_VALID] != 0;
                    if valid && (raw != ai.beh_raw || (written && netai::has_fresh(&raw))) {
                        let mut wire = raw.clone();
                        let pids = player_ids.get_or_insert_with(|| {
                            mode4::player_targets(player, peers, ghost)
                                .into_iter()
                                .filter_map(|(c, id)| Some((netai::net_id_of(c)?, id)))
                                .collect()
                        });
                        if !target_to_wire(&mut wire, &by_net, pids) {
                            st.unmapped_targets += 1;
                        }
                        // The first record we see may be minutes old: send
                        // its states without "fresh", so the client's enemy
                        // takes the state without replaying an old attack.
                        if ai.beh_raw.is_empty() {
                            netai::clear_fresh(&mut wire);
                        }
                        ai.beh_raw = raw;
                        ai.beh_wire = Some(wire);
                        ai.beh_seq = ai.beh_seq.wrapping_add(1);
                        ai.beh_left = NETAI_SENDS;
                        ai.beh_new = true;
                        st.beh_records += 1;
                    }
                    if written {
                        netai::clear_flags(&t, netai::F_BEHAVIOR_WRITTEN);
                    }
                }
                if let Some(raw) = netai::read_record(t.action, NETAI_ACTION_LEN)
                    && raw != ai.act
                    && !(ai.act.is_empty() && raw.iter().all(|&b| b == 0))
                {
                    ai.act = raw;
                    ai.act_seq = ai.act_seq.wrapping_add(1);
                    ai.act_left = NETAI_SENDS;
                    ai.act_new = true;
                    st.act_records += 1;
                }
            }
            if ai.beh_left > 0 {
                e.behavior = ai.beh_wire.clone();
            }
            if ai.act_left > 0 {
                e.action = Some(ai.act.clone());
            }
            e.beh_seq = ai.beh_seq;
            e.act_seq = ai.act_seq;
            if e.movement.is_none() && e.behavior.is_none() && e.action.is_none() {
                continue;
            }
            let class = if ai.beh_new && e.behavior.is_some() || ai.act_new && e.action.is_some() {
                0
            } else if near && e.movement.is_some() {
                1
            } else if e.behavior.is_some() || e.action.is_some() {
                2
            } else {
                3
            };
            cands.push((class, dist, e, id));
        }
        cands.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));

        let mut used = 0;
        let mut list = Vec::with_capacity(cands.len());
        for (_, _, e, id) in cands {
            let len = e.wire_len();
            if used + len > NETAI_TICK_BYTES && !list.is_empty() {
                // Records stay due (left/new untouched) for the next tick.
                self.netai_stats.deferred += 1;
                continue;
            }
            used += len;
            if let Some(ai) = self.host_ai.get_mut(&id) {
                if e.behavior.is_some() {
                    ai.beh_left = ai.beh_left.saturating_sub(1);
                    ai.beh_new = false;
                }
                if e.action.is_some() {
                    ai.act_left = ai.act_left.saturating_sub(1);
                    ai.act_new = false;
                }
            }
            list.push(e);
        }
        net.send_netai(&list);
        self.sent_ids = sent_ids;
        // Forget enemies no longer known (rescans replace `known`).
        if tick % 250 == 0 {
            let known: HashSet<i32> = self.known.iter().map(|(id, _)| *id).collect();
            self.host_ai.retain(|id, _| known.contains(id));
        }

        if self.netai_stats_at.is_none_or(|t| now.duration_since(t) >= STATS_EVERY) {
            let secs = self.netai_stats_at.map_or(STATS_EVERY.as_secs_f32(), |t| now.duration_since(t).as_secs_f32());
            self.netai_stats_at = Some(now);
            let s = std::mem::take(&mut self.netai_stats);
            let bytes = net.netai_bytes();
            let ticks = (secs / NETAI_EVERY.as_secs_f32()).max(1.0);
            log::line(&format!(
                "netai host: per tick {:.1} enemies in range, {:.1} with netId, {:.1} with a table entry; move flag 0x02 set on {}/{} reads; new records behavior {} action {}; unmapped targets {}; deferred {}; {:.1} KB/s",
                s.in_range as f32 / ticks,
                s.with_net_id as f32 / ticks,
                s.with_entry as f32 / ticks,
                s.move_written_seen,
                s.move_reads,
                s.beh_records,
                s.act_records,
                s.unmapped_targets,
                s.deferred,
                (bytes - s.bytes_at) as f32 / 1024.0 / secs.max(0.001)
            ));
            self.netai_stats.bytes_at = bytes;
        }
    }

    /// Client: only the host is the enemy authority; anything else is dropped (counted
    /// in the stats line; clients never send enemy data, so this stays 0).
    fn from_host<T>(&mut self, list: Vec<(PeerId, T)>) -> Vec<T> {
        let n = list.len();
        let v: Vec<T> = list.into_iter().filter(|(from, _)| from.is_host()).map(|(_, x)| x).collect();
        self.not_host_rx += (n - v.len()) as u32;
        v
    }

    fn client_frame(&mut self, net: &mut Net, peers: &PeerSet, player: Chr) {
        let now = Instant::now();
        let mode = tuning::enemy_mode();
        let states = net.take_enemy_states_from();
        for s in self.from_host(states) {
            if !game::sane_pos(s.pos) {
                continue;
            }
            self.latest.insert(s.id, (now, s));
            let h = self.history.entry(s.id).or_default();
            h.push_back((now, s.pos));
            while h.len() > 8 {
                h.pop_front();
            }
        }
        // NetAI entries: HP (and position, for the fallback and the stale
        // check) feed the same per-enemy state; records wait for the write.
        let records = net.take_netai_from();
        for e in self.from_host(records) {
            let pos = e.movement.as_deref().and_then(netai::move_pos).filter(|&p| game::sane_pos(p));
            let ai = self.client_ai.entry(e.id).or_default();
            if let Some(m) = e.movement {
                ai.movement = Some(m);
                ai.seen_move = true;
            }
            if let Some(b) = e.behavior
                && ai.beh_seq != Some(e.beh_seq)
            {
                ai.beh_seq = Some(e.beh_seq);
                ai.behavior = Some(b);
            }
            if let Some(a) = e.action
                && ai.act_seq != Some(e.act_seq)
            {
                ai.act_seq = Some(e.act_seq);
                ai.action = Some(a);
            }
            let prev = self.latest.get(&e.id).map(|(_, s)| s.pos);
            let Some(p) = pos.or(prev) else { continue };
            let angle = ai.movement.as_deref().map_or(0.0, netai::move_yaw);
            self.latest.insert(e.id, (now, EnemyState { id: e.id, pos: p, angle, hp: e.hp, anim: 0 }));
            if pos.is_some() {
                let h = self.history.entry(e.id).or_default();
                h.push_back((now, p));
                while h.len() > 8 {
                    h.pop_front();
                }
            }
        }

        // Drop enemies the host stopped sending.
        let stale: Vec<i32> =
            self.latest.iter().filter(|(_, (t, _))| now.duration_since(*t) > STALE_AFTER).map(|(id, _)| *id).collect();
        for id in stale {
            self.latest.remove(&id);
            self.history.remove(&id);
            self.smooth.remove(&id);
            self.lethal_at.remove(&id);
            self.last_written_hp.remove(&id);
            self.last_anim.remove(&id);
            if let Some(slot) = self.slots.remove(&id) {
                hook::clear_target(slot);
            }
            let alive = game::chr_by_entity_id(id);
            if let Some(ai) = self.client_ai.remove(&id)
                && let Some(c) = ai.engaged
            {
                netai::release(c, alive == Some(c));
            }
            if self.no_move.remove(&id)
                && let Some(c) = alive
            {
                netai::set_no_move(c, false);
            }
        }

        let smoothing = tuning::enemy_smoothing();
        self.owned.clear();
        let ids: Vec<i32> = self.latest.keys().copied().collect();
        for id in ids {
            let (_, s) = self.latest[&id];
            // Enemy not loaded here (or physics gone): free its slot so the hook
            // never keeps a stale physics pointer.
            let resolved = game::chr_by_entity_id(id).and_then(|c| Some((c, c.physics()?, c.position4()?)));
            let Some((chr, physics, cur4)) = resolved else {
                self.smooth.remove(&id);
                if let Some(slot) = self.slots.remove(&id) {
                    hook::clear_target(slot);
                }
                if let Some(ai) = self.client_ai.get_mut(&id)
                    && let Some(c) = ai.engaged.take()
                {
                    netai::release(c, false);
                }
                self.no_move.remove(&id);
                continue;
            };

            // HP: our local loss since the last copy is our damage.
            let local = chr.hp().unwrap_or(0);
            // Only an enemy we saw alive here for a while can take our damage.
            // Test 2026-10-22: enemies the client had killed before joining
            // dropped back to 0 right after we copied the host's HP, which was
            // counted as hits and killed them on the host too.
            let alive_before = self.alive_frames.get(&id).copied().unwrap_or(0) >= ALIVE_FRAMES_FOR_DAMAGE;
            if local > 0 {
                *self.alive_frames.entry(id).or_insert(0) += 1;
            } else {
                self.alive_frames.insert(id, 0);
            }
            // Posture and deathblows (posture.rs), counted BEFORE the HP report
            // below: a deathblow and the HP it took leave in the same report,
            // so the host takes the life first (else its HP floor of 1 keeps a
            // last-life enemy alive).
            let mut posture_report = crate::posture::enabled() && {
                let fighting = self.last_hit.get(&id).is_some_and(|t| now.duration_since(*t) < OWN_COMBAT_FOR);
                self.posture.client_step(id, chr, alive_before, fighting, now)
            };
            // The game clamps the HP we copied to our max (scaling not applied yet,
            // a different NG cycle): that drop is not our damage (SPEC-DEBUFF-SCALING §3.5).
            let max_hp = game::read_ptr(chr.0 + 0x1FF8).and_then(|m| game::read_ptr(m + 0x18)).and_then(|d| game::read::<i32>(d + 0x134));
            if let Some(&written) = self.last_written_hp.get(&id)
                && local < written
                && max_hp.is_some_and(|m| written > m && local == m)
            {
                self.last_written_hp.insert(id, local);
                if self.clamp_logs < 20 {
                    self.clamp_logs += 1;
                    log::line(&format!("enemies: hp clamp {id} host {} max {max_hp:?}", s.hp));
                }
            } else if let Some(&written) = self.last_written_hp.get(&id)
                && local < written
                && alive_before
            {
                let total = self.dealt_total.entry(id).or_insert(0);
                *total += written - local;
                net.send_damage_to(Dest::Host, self.posture.report_hp(id, *total));
                posture_report = false;
                self.last_hit.insert(id, now);
                if local <= 0 {
                    self.lethal_at.insert(id, now);
                }
                // Counted: a held (not rewritten) HP must not count again.
                self.last_written_hp.insert(id, local);
            }
            // Posture-only change (a guarded hit, a deflect): its own report.
            if posture_report && let Some(r) = self.posture.report(id, self.dealt_total.get(&id).copied()) {
                net.send_damage_to(Dest::Host, r);
            }
            let recently_hit = self.last_hit.get(&id).is_some_and(|t| now.duration_since(*t) < OWN_COMBAT_FOR);
            // Our killing blow: keep the enemy dead until the host agrees or
            // LETHAL_HOLD passes (writing its HP back mid-death broke it).
            let lethal_hold = local <= 0
                && s.hp > 0
                && self.lethal_at.get(&id).is_some_and(|t| now.duration_since(*t) < LETHAL_HOLD);
            // The local game owns position and animation while we fight it and
            // while it is dead on both sides (death animation plays out).
            let in_own_combat = recently_hit || lethal_hold || (local <= 0 && s.hp <= 0);
            // While we fight it the host's HP lags our hits by a round trip:
            // raising HP back mid-combo would undo them on screen. Never when
            // it is dead here but alive on the host (boss phase refill, revive).
            let holding = lethal_hold || (recently_hit && local > 0 && local < s.hp);
            // Dead here, alive on the host: the game cannot revive it by HP, and
            // writing HP to a corpse is what produced the phantom kills above.
            let corpse = local <= 0 && s.hp > 0 && !lethal_hold;
            // Rest = reset for both (restsync.rs): our rest respawned it, the host's
            // respawn of the same rest has not reached us yet. Writing the host's 0 here
            // killed it again for good (live 2026-10-23, the friend's rests).
            // The same for a damaged state: a freshly respawned (full) copy here, a lower
            // HP on the host = the host's copy from before its respawn (live 2026-10-24:
            // 1120300 got the host's pre-rest 4743 HP and 1 life onto its fresh copy, and
            // livesync then took that life back on the host). Persist/posture §35.
            let fresh_here = max_hp.is_some_and(|m| m > 0 && local >= m);
            let rest_hold = local > 0 && (s.hp <= 0 || (fresh_here && s.hp < local)) && crate::restsync::hold_kills();
            if rest_hold && self.rest_held.insert(id) && self.rest_logs < 10 {
                self.rest_logs += 1;
                log::line(&format!(
                    "rest: enemy {id} respawned here, {} on the host: not lowered (waiting for the host's respawn)",
                    if s.hp <= 0 { "still dead".to_string() } else { format!("HP {} (from before its respawn?)", s.hp) }
                ));
            }
            // v8.60: a Puppeteer puppet here is our own fighter; the host's copy of that enemy
            // is a corpse. Held like a rest-held enemy: no host HP (0 killed it), its own AI,
            // no position from the host (puppetguard.rs covers the native session's type 5).
            let rest_hold = rest_hold || crate::puppetguard::mode4_hold(chr.0, id, local, s.hp);
            if corpse || rest_hold {
                self.last_written_hp.insert(id, local);
            } else if !holding {
                if local != s.hp {
                    chr.set_hp(s.hp);
                }
                // What the game kept (it may clamp), so a clamp is not
                // mistaken for our damage next frame.
                self.last_written_hp.insert(id, chr.hp().unwrap_or(s.hp));
            }

            if in_own_combat || corpse || rest_hold {
                self.owned.insert(id);
            }

            // NetAI: host-driven while alive here. A dead enemy (our killing
            // blow, a corpse, dead on both sides) goes back to its own AI so
            // the local death / deathblow plays out (RE-FINDINGS-2 live check
            // 5). Being hit does NOT release it: that would bring back the
            // "two AIs" fight the mode exists to end.
            // Throw window (RE-BACKSTAB.md patch A): while we stand behind it or a
            // throw is running on it, the enemy gets its own AI back here, so the
            // game's stealth / deathblow gates and the throw itself work as offline.
            let handed_off = mode == tuning::ENEMY_NETAI && local > 0 && self.throw_window(id, chr, player, now);
            // A rest-held enemy keeps its own AI at its spawn point: the host's records
            // still describe the corpse.
            let netai_driven = if mode == tuning::ENEMY_NETAI && local > 0 && !handed_off && !rest_hold {
                self.netai_step(id, chr, player, peers, now)
            } else {
                if let Some(ai) = self.client_ai.get_mut(&id)
                    && let Some(c) = ai.engaged.take()
                {
                    netai::release(c, c == chr);
                }
                false
            };

            // Position: smooth towards the host's, snap on big jumps. Not while
            // we fight it ourselves (a deathblow moves both characters).
            let hook_mode = tuning::enemy_hook_positions() || (mode == tuning::ENEMY_NETAI && !netai_driven);
            let by_hook = hook_mode && !in_own_combat && !corpse && !handed_off && !rest_hold;
            if !by_hook {
                self.smooth.remove(&id);
                if let Some(slot) = self.slots.remove(&id) {
                    hook::clear_target(slot);
                }
            } else {
                let prev = self.smooth.get(&id).copied().unwrap_or([cur4[0], cur4[1], cur4[2]]);
                let delay = tuning::enemy_delay_ms();
                let interp = (delay > 0).then(|| self.history.get(&id).and_then(|h| interpolate(h, now, delay))).flatten();
                let next = if let Some(p) = interp {
                    // Snapshot interpolation: exact path, fixed delay (no chase).
                    p
                } else if game::distance(prev, s.pos) > SNAP_DISTANCE_M {
                    s.pos
                } else {
                    [
                        prev[0] + (s.pos[0] - prev[0]) * smoothing,
                        prev[1] + (s.pos[1] - prev[1]) * smoothing,
                        prev[2] + (s.pos[2] - prev[2]) * smoothing,
                    ]
                };
                self.smooth.insert(id, next);
                if let Some(slot) = self.slot_for(id) {
                    hook::set_target(slot, physics, [next[0], next[1], next[2], cur4[3]]);
                }
            }

            // Fallback option: the enemy's own AI must not walk it away from
            // the hooked position (it still thinks and attacks).
            let want_no_move = by_hook && tuning::enemy_no_move();
            if want_no_move != self.no_move.contains(&id) && netai::set_no_move(chr, want_no_move) {
                if want_no_move {
                    self.no_move.insert(id);
                } else {
                    self.no_move.remove(&id);
                }
            }

            // Animation: mode 3 mirrors the host's behavior events (hkbsync.rs).
            // The old per-id play sent TimeAct ids the game could not use.
        }

        // End of the rest window: how the host's respawn caught up.
        if !self.rest_held.is_empty() && !crate::restsync::hold_kills() {
            let held = std::mem::take(&mut self.rest_held);
            let alive = held.iter().filter(|id| self.latest.get(id).is_some_and(|(_, s)| s.hp > 0)).count();
            log::line(&format!(
                "rest: window over: {} enemies were alive here and dead or lower on the host after the rest; the host now reports {alive} of them alive, {} still dead (the host's HP decides again)",
                held.len(),
                held.len() - alive
            ));
        }

        // Damage packets are unreliable: re-send recent totals for a few seconds
        // so a dropped killing blow still lands on the host.
        if self.last_resend.is_none_or(|t| now.duration_since(t) >= RESEND_DAMAGE_EVERY) {
            self.last_resend = Some(now);
            self.last_hit.retain(|_, t| now.duration_since(*t) < RESEND_DAMAGE_FOR);
            let mut ids: Vec<i32> = self.last_hit.keys().copied().collect();
            for id in self.posture.recent(now) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            for id in ids {
                // HP total plus the posture / deathblow totals (posture.rs).
                if let Some(r) = self.posture.report(id, self.dealt_total.get(&id).copied()) {
                    net.send_damage_to(Dest::Host, r);
                }
            }
        }

        if self.stats_at.is_none_or(|t| t.elapsed().as_secs() >= 20) {
            self.stats_at = Some(Instant::now());
            log::line(&format!(
                "enemies: client mirroring {} enemies ({} hook slots){}",
                self.latest.len(),
                self.slots.len(),
                if self.not_host_rx > 0 { format!("; {} entries from a non-host peer dropped", std::mem::take(&mut self.not_host_rx)) } else { String::new() }
            ));
            let (mut dead_here, mut dead_there) = (Vec::new(), Vec::new());
            for (id, (_, s)) in &self.latest {
                if let Some(c) = game::chr_by_entity_id(*id) {
                    let l = c.hp().unwrap_or(0);
                    if l <= 0 && s.hp > 0 {
                        dead_here.push(*id);
                    } else if l > 0 && s.hp <= 0 {
                        dead_there.push(*id);
                    }
                }
            }
            log::line(&format!(
                "enemies: HP mismatch: dead here / alive on host {dead_here:?}, alive here / dead on host {dead_there:?}"
            ));
            if mode == tuning::ENEMY_NETAI {
                let s = std::mem::take(&mut self.netai_stats);
                let engaged = self.client_ai.values().filter(|a| a.engaged.is_some()).count();
                log::line(&format!(
                    "netai client: {engaged} host-driven; NetAI active (+0x58 == +0x2008) on {}/{} checked frames; written move {} behavior {} action {} (bad behavior {}); hook-fallback enemy frames {}; game flipped to AI {} other {}, enforced {}, slot-37 calls {}",
                    s.active_frames,
                    s.checked_frames,
                    s.moves_written,
                    s.behs_written,
                    s.acts_written,
                    s.behs_bad,
                    s.fallbacks,
                    s.flipped_ai,
                    s.flipped_other,
                    s.enforced,
                    netai::PICK_CALLS.swap(0, std::sync::atomic::Ordering::Relaxed)
                ));
                // Keep the once-only log counters.
                self.netai_stats.engaged_logs = s.engaged_logs;
                self.netai_stats.inactive_logs = s.inactive_logs;
            }
        }
    }

    /// Client, mode 4: keeps `chr` on its NetAIManipulator and writes the
    /// host's records into its table entry. False = not host-driven (the
    /// caller uses the hook fallback).
    fn netai_step(&mut self, id: i32, chr: Chr, player: Chr, peers: &PeerSet, now: Instant) -> bool {
        let Some(ai) = self.client_ai.get_mut(&id) else { return false };
        // Only once the host sent a movement record: the NetAI side without
        // records just stands.
        if !ai.seen_move {
            return false;
        }
        let st = &mut self.netai_stats;
        // The pointer changed (respawn): the old object is gone.
        if let Some(old) = ai.engaged
            && old != chr
        {
            netai::release(old, false);
            ai.engaged = None;
        }
        if ai.engaged.is_none() {
            if ai.refused_at.is_some_and(|t| now.duration_since(t) < NETAI_RETRY) {
                st.fallbacks += 1;
                return false;
            }
            match netai::engage(chr) {
                Ok(()) => {
                    ai.engaged = Some(chr);
                    ai.refused_at = None;
                    st.engaged_logs += 1;
                    if st.engaged_logs <= 20 {
                        log::line(&format!(
                            "netai client: enemy {id} now host-driven (netId {:?}, netai manip {:?})",
                            netai::net_id(chr).map(|n| format!("{n:#x}")),
                            netai::netai_manip(chr).map(|p| format!("{p:#x}"))
                        ));
                    }
                }
                Err(e) => {
                    ai.refused_at = Some(now);
                    if !ai.refuse_logged {
                        ai.refuse_logged = true;
                        log::line(&format!("netai client: enemy {id} stays on hook positions: {e}"));
                    }
                    st.fallbacks += 1;
                    return false;
                }
            }
        }
        // The game asked for this enemy's local AI (a script): let it have it.
        if netai::forced_local(chr) {
            netai::release(chr, true);
            ai.engaged = None;
            ai.refused_at = Some(now);
            st.fallbacks += 1;
            return false;
        }
        let Some(t) = netai::net_id(chr).and_then(netai::entry) else {
            // Table block gone (map block unloading): back to the hook.
            netai::release(chr, true);
            ai.engaged = None;
            ai.refused_at = Some(now);
            return false;
        };

        let mut bits = 0;
        if let Some(m) = ai.movement.take()
            && netai::write_record(t.movement, &m)
        {
            bits |= netai::F_MOVE_PENDING;
            st.moves_written += 1;
        }
        if let Some(mut b) = ai.behavior.take() {
            let follow = target_from_wire(&mut b, player, peers);
            if netai::behavior_sane(&b) && netai::write_record(t.behavior, &b) {
                ai.follow = follow;
                bits |= netai::F_BEHAVIOR_PENDING;
                st.behs_written += 1;
            } else {
                st.behs_bad += 1;
            }
        }
        if let Some(a) = ai.action.take()
            && netai::write_record(t.action, &a)
        {
            bits |= netai::F_ACTION_PENDING;
            st.acts_written += 1;
        }
        // Data first, the pending bits last (same thread as the reader).
        if bits != 0 {
            netai::set_flags(&t, bits);
        }
        // A player target moves: keep the turn point on them.
        if let Some(p) = ai.follow.and_then(|id| mode4::player_pos(id, peers, player.position())) {
            netai::refresh_target_pos(chr, p);
        }

        // Live check 3: after the swap the NetAI side stays active. Counted
        // before we enforce it, so it shows how often the game flips it back.
        let kind = netai::active_kind(chr);
        if kind != 3 {
            st.checked_frames += 1;
            match kind {
                0 => st.active_frames += 1,
                1 => st.flipped_ai += 1,
                _ => st.flipped_other += 1,
            }
            if kind != 0 {
                if st.inactive_logs < 10 {
                    st.inactive_logs += 1;
                    log::line(&format!("netai client: enemy {id} host-driven but +0x58 is {} (not NetAI)", if kind == 1 { "the AI manipulator" } else { "another object" }));
                }
                // Every frame from our own task, not only when the game asks.
                netai::switch_to_net(chr.0);
                st.enforced += 1;
            }
        }
        true
    }

    fn slot_for(&mut self, id: i32) -> Option<usize> {
        if let Some(&s) = self.slots.get(&id) {
            return Some(s);
        }
        let used: Vec<usize> = self.slots.values().copied().collect();
        // The top slots belong to the native remotes' position pins (native.rs, one per peer).
        let free = (1..crate::native::PIN_SLOT_BASE).find(|s| !used.contains(s))?;
        self.slots.insert(id, free);
        Some(free)
    }
}

/// Host: rewrites the behavior record's AI target into the wire form (an enemy by
/// entity id, a player by session slot: `players` = (host netId, slot)).
/// False if a character target could not be mapped (sent as "none").
fn target_to_wire(rec: &mut [u8], by_net: &HashMap<u16, i32>, players: &[(u16, PeerId)]) -> bool {
    match netai::get_i32(rec, netai::BEH_TARGET_MODE) {
        netai::TARGET_CHR => {
            let nid = netai::get_i32(rec, netai::BEH_TARGET) as u16;
            let (code, val) = if let Some(&entity) = by_net.get(&nid) {
                (netai::WIRE_ENEMY, entity)
            } else if let Some(id) = mode4::player_of_net_id(nid, players) {
                (mode4::wire_player(id), 0)
            } else {
                netai::put_i32(rec, netai::BEH_TARGET_MODE, netai::TARGET_NONE);
                return false;
            };
            netai::put_i32(rec, netai::BEH_TARGET_MODE, code);
            netai::put_i32(rec, netai::BEH_TARGET, val);
            true
        }
        netai::TARGET_POS => true,
        _ => {
            netai::put_i32(rec, netai::BEH_TARGET_MODE, netai::TARGET_NONE);
            true
        }
    }
}

/// Client: wire target -> the game's form (never leaves a wire code in the
/// record). Players (ourselves or any peer, by slot) become a position target
/// refreshed every frame; an enemy becomes a chr target with the client's own
/// netId for it. Returns the player to follow.
fn target_from_wire(rec: &mut [u8], player: Chr, peers: &PeerSet) -> Option<PeerId> {
    let pos_target = |rec: &mut [u8], p: Option<[f32; 3]>| match p.filter(|&p| game::sane_pos(p)) {
        Some(p) => {
            netai::put_i32(rec, netai::BEH_TARGET_MODE, netai::TARGET_POS);
            netai::put_pos(rec, netai::BEH_TARGET, p);
            true
        }
        None => {
            netai::put_i32(rec, netai::BEH_TARGET_MODE, netai::TARGET_NONE);
            false
        }
    };
    let code = netai::get_i32(rec, netai::BEH_TARGET_MODE);
    if let Some(id) = mode4::wire_player_id(code) {
        let p = mode4::player_pos(id, peers, player.position());
        return pos_target(rec, p).then_some(id);
    }
    match code {
        netai::WIRE_ENEMY => {
            let entity = netai::get_i32(rec, netai::BEH_TARGET);
            match game::chr_by_entity_id(entity).and_then(netai::net_id) {
                Some(nid) => {
                    netai::put_i32(rec, netai::BEH_TARGET_MODE, netai::TARGET_CHR);
                    // u16 netId, upper half cleared.
                    netai::put_i32(rec, netai::BEH_TARGET, nid as i32);
                }
                None => {
                    let p = game::chr_by_entity_id(entity).and_then(|c| c.position());
                    pos_target(rec, p);
                }
            }
            None
        }
        netai::TARGET_POS => None,
        _ => {
            netai::put_i32(rec, netai::BEH_TARGET_MODE, netai::TARGET_NONE);
            None
        }
    }
}

/// Position `delay_ms` in the past from timed samples (linear); None if the
/// samples do not reach back that far yet.
fn interpolate(h: &std::collections::VecDeque<(Instant, [f32; 3])>, now: Instant, delay_ms: u32) -> Option<[f32; 3]> {
    let t = now.checked_sub(Duration::from_millis(delay_ms as u64))?;
    let mut prev: Option<&(Instant, [f32; 3])> = None;
    for e in h {
        if e.0 >= t {
            let a = prev?;
            // A respawn/teleport between two updates: jump, don't glide.
            if game::distance(a.1, e.1) > SNAP_DISTANCE_M {
                return Some(e.1);
            }
            let span = e.0.duration_since(a.0).as_secs_f32().max(1e-4);
            let k = (t.duration_since(a.0).as_secs_f32() / span).clamp(0.0, 1.0);
            return Some([0, 1, 2].map(|i| a.1[i] + (e.1[i] - a.1[i]) * k));
        }
        prev = Some(e);
    }
    h.back().map(|e| e.1)
}

impl Enemies {
    /// Sync trace for comparing both machines afterwards: once a second one
    /// log line with the unix time (ms), our player, the friend (as we know
    /// it), the ghost, and the 14 nearest enemies (entity id: x,y,z,local hp).
    /// Match host and client lines by entity id and time.
    fn trace(&mut self, net: &Net, player: Chr, friend_pos: Option<[f32; 3]>, ghost: Option<Chr>) {
        let now = Instant::now();
        if self.trace_at.is_some_and(|t| now.duration_since(t) < Duration::from_secs(1)) {
            return;
        }
        self.trace_at = Some(now);
        let Some(pp) = player.position() else { return };
        let ids: Vec<i32> = if native_owns_enemies() {
            // Native session: `known` / `latest` stay empty, take the loaded enemies.
            crate::nsession::enemy_chrs().into_iter().filter_map(|c| c.entity_id()).collect()
        } else if net.is_host() {
            self.known.iter().map(|(id, _)| *id).collect()
        } else {
            self.latest.keys().copied().collect()
        };
        let mut near: Vec<(f32, String)> = Vec::new();
        for id in ids {
            if let Some(c) = game::chr_by_entity_id(id)
                && let Some(p) = c.position()
            {
                let d = game::distance(pp, p);
                if d < 60.0 {
                    near.push((d, format!("{id}:{:.1},{:.1},{:.1},{}", p[0], p[1], p[2], c.hp().unwrap_or(-1))));
                }
            }
        }
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        let list: Vec<String> = near.into_iter().take(14).map(|(_, s)| s).collect();
        let fmt = |p: Option<[f32; 3]>| p.map_or("-".to_string(), |p| format!("{:.1},{:.1},{:.1}", p[0], p[1], p[2]));
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        log::line(&format!(
            "T {} ms={ms} P={} F={} G={} E={}",
            if net.is_host() { "host" } else { "client" },
            fmt(Some(pp)),
            fmt(friend_pos),
            fmt(ghost.and_then(|g| g.position())),
            list.join(";")
        ));
    }
}

/// Which way an `angle` (physics yaw, radians) points in the xz plane. The
/// convention is not documented, so it is learned from the local player:
/// while running, the movement direction is compared with the 8 candidate
/// mappings and the best-scoring one wins.
#[derive(Default)]
struct Facing {
    prev: Option<[f32; 3]>,
    score: [f32; 8],
    samples: u32,
}

impl Facing {
    fn candidates(a: f32) -> [[f32; 2]; 8] {
        let (s, c) = a.sin_cos();
        [[s, c], [-s, c], [s, -c], [-s, -c], [c, s], [-c, s], [c, -s], [-c, -s]]
    }

    fn learn(&mut self, pos: [f32; 3], angle: f32) {
        if let Some(p) = self.prev {
            let (dx, dz) = (pos[0] - p[0], pos[2] - p[2]);
            let len = (dx * dx + dz * dz).sqrt();
            // 0.02..1.0 m per frame (120 fps halves it): walking or running, not a teleport.
            if (0.02..1.0).contains(&len) {
                for (score, f) in self.score.iter_mut().zip(Self::candidates(angle)) {
                    *score += (f[0] * dx + f[1] * dz) / len;
                }
                self.samples += 1;
            }
        }
        self.prev = Some(pos);
    }

    fn best(&self) -> Option<usize> {
        (self.samples >= 30).then(|| {
            (0..8).max_by(|&a, &b| self.score[a].total_cmp(&self.score[b])).unwrap_or(0)
        })
    }

    fn forward(&self, angle: f32) -> Option<[f32; 2]> {
        Some(Self::candidates(angle)[self.best()?])
    }
}

/// Stealth window (settings `[GAMEPLAY] stealth_window_m`, IMPLEMENTATION-NOTES §27):
/// an enemy we approach from behind (rear 120 degrees) or from above within this many
/// metres (horizontal) runs on our own AI (mode 4) / is claimed for us (native session),
/// so its awareness is our machine's, as in single player. 0 = the old 5 m rear check.
pub static STEALTH_WINDOW_M: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(15);
/// Plunging deathblow: at least this far above the enemy, at most `PLUNGE_MAX_ABOVE_M`.
const PLUNGE_ABOVE_M: f32 = 2.0;
const PLUNGE_MAX_ABOVE_M: f32 = 25.0;
/// Plunge approaches count within at most this horizontal distance.
const PLUNGE_MAX_HORIZ_M: f32 = 12.0;
/// Once handed off, the enemy stays on our AI this long after the approach ended
/// (a patrol turning round must not flap it between the two AIs).
const STEALTH_LINGER: Duration = Duration::from_millis(3000);
/// While handed off, the "behind" test widens to the rear half and this much farther.
const STEALTH_HOLD_EXTRA_M: f32 = 5.0;
/// Native session: a claim is dropped this long after the approach ended.
const CLAIM_LINGER: Duration = Duration::from_millis(3000);

fn stealth_window_m() -> f32 {
    STEALTH_WINDOW_M.load(std::sync::atomic::Ordering::Relaxed).min(40) as f32
}

/// `d` = player minus enemy position, `forward` = the enemy's facing in the xz plane
/// (learned mapping, `Facing`), `r` = the stealth window. "above": a plunging deathblow
/// approach (2..25 m higher, within min(reach, 12 m) horizontally, any direction);
/// "behind": within `r` horizontally in the rear 120 degrees (the rear half and 5 m more
/// while `holding`). `r <= 0` = the old rule: 5 m (3D) in the rear 120 degrees.
fn approach_kind(d: [f32; 3], forward: Option<[f32; 2]>, r: f32, holding: bool) -> Option<&'static str> {
    let horiz = (d[0] * d[0] + d[2] * d[2]).sqrt();
    let dot = |f: [f32; 2]| (f[0] * d[0] + f[1] * d[2]) / horiz.max(1e-3);
    if r <= 0.0 {
        let dist = (horiz * horiz + d[1] * d[1]).sqrt();
        return (dist < 5.0 && forward.is_some_and(|f| dot(f) < -0.5)).then_some("behind");
    }
    let reach = if holding { r + STEALTH_HOLD_EXTRA_M } else { r };
    if horiz <= reach.min(PLUNGE_MAX_HORIZ_M) && (PLUNGE_ABOVE_M..=PLUNGE_MAX_ABOVE_M).contains(&d[1]) {
        return Some("above");
    }
    let cone = if holding { 0.0 } else { -0.5 };
    (horiz <= reach && d[1].abs() < PLUNGE_MAX_ABOVE_M && forward.is_some_and(|f| dot(f) < cone)).then_some("behind")
}

/// May the stealth approach `reason` ("behind" / "above") hand this enemy to the local AI?
/// Not for a boss: its deathblow window comes from the posture / throw node reasons, which
/// stay allowed.
fn boss_handoff_allowed(is_boss: bool, reason: &str) -> bool {
    !(is_boss && matches!(reason, "behind" | "above"))
}

/// Starts with two or more lives (NpcParam deathblows): a boss or a miniboss, also one that
/// is missing from the bosses table and shows no gauge here (Headless, live 2026-10-27).
fn boss_like_lives(chr: Chr) -> bool {
    crate::livesync::multi_life(crate::livesync::lives0_of(chr))
}

/// Another living player stands within `FIGHT_NEAR_M` of an enemy at `ep`: it is in a
/// fight with them, not a stealth target of ours.
fn fought_by_another(ep: [f32; 3], others: &[[f32; 3]]) -> bool {
    others.iter().any(|&o| game::distance(ep, o) <= FIGHT_NEAR_M)
}

/// v8.10: 8 m let the friend take enemies the host fought from 10-20 m (from a roof, from
/// below): 123 "claimed by both machines" in 18 min, live 2026-10-25.
const FIGHT_NEAR_M: f32 = 20.0;

/// The enemy's posture bar is empty here (counts down; `data+0x148`, posture.rs): the
/// local stagger / deathblow window runs.
fn posture_broken(chr: Chr) -> bool {
    let Some(d) = game::read_ptr(chr.0 + 0x1FF8).and_then(|m| game::read_ptr(m + 0x18)) else { return false };
    let (Some(cur), Some(max)) = (game::read::<i32>(d + 0x148), game::read::<i32>(d + 0x14C)) else { return false };
    max > 0 && cur <= 0
}

impl Enemies {
    /// The local player's approach on `chr` as a stealth deathblow ("behind") or a
    /// plunging deathblow ("above"). `holding`: already handed off / claimed (wider test).
    /// Returns the reason and the height difference (player minus enemy).
    fn stealth_approach(&mut self, chr: Chr, player: Chr, holding: bool) -> Option<(&'static str, f32)> {
        let (pp, ep) = (player.position()?, chr.position()?);
        if let Some(a) = player.angle() {
            self.facing.learn(pp, a);
        }
        let d = [pp[0] - ep[0], pp[1] - ep[1], pp[2] - ep[2]];
        let forward = chr.angle().and_then(|a| self.facing.forward(a));
        approach_kind(d, forward, stealth_window_m(), holding).map(|k| (k, d[1]))
    }

    /// Client, NetAI enemies (mode 4): true while the enemy is handed to its local AI
    /// for a stealth / plunging deathblow or a throw. Enters on a stealth approach
    /// (`stealth_approach`: within `stealth_window_m` behind it, or above it), an active
    /// throw node, or an empty posture bar (both within 8 m); stays while any of them
    /// holds (wider test) and leaves `STEALTH_LINGER` after the last one.
    /// The old window (5 m behind, live 2026-10-23) gave the local AI no time: it took
    /// over the host's alert behavior a step before the strike, so the stealth gates
    /// (unaware state) were not met; plunges from above were never handed off.
    fn throw_window(&mut self, id: i32, chr: Chr, player: Chr, now: Instant) -> bool {
        let (Some(pp), Some(ep)) = (player.position(), chr.position()) else { return false };
        let dist = game::distance(pp, ep);
        let holding = self.handoff.contains_key(&id);
        let approach = self.stealth_approach(chr, player, holding);
        // Only throws on enemies near us matter (and are ours).
        let node = dist < 8.0 && throw_active(chr);
        let broken = dist < 8.0 && posture_broken(chr);
        let reason = if node {
            Some("node")
        } else if broken {
            Some("posture")
        } else {
            // A boss (or an illusion of its encounter) never goes to the friend's own AI
            // for a stealth / plunge approach: two separate bosses ran, 22 m apart (live
            // 2026-10-26, 6 handoffs in 156 s). The native path has had this since v8.6.
            let is_boss = approach.is_some()
                && (crate::bosses::encounter_of(id).is_some()
                    || game::boss_state().is_some_and(|b| b.entity == id)
                    || boss_like_lives(chr));
            approach.map(|(r, _)| r).filter(|r| boss_handoff_allowed(is_boss, r))
        };
        let old_rule = stealth_window_m() <= 0.0;
        match self.handoff.get_mut(&id) {
            None => {
                let Some(why) = reason else { return false };
                if let Some(ai) = self.client_ai.get_mut(&id)
                    && let Some(c) = ai.engaged.take()
                {
                    netai::release(c, c == chr);
                }
                self.handoff.insert(id, (now, now));
                log::line(&format!(
                    "throw: handoff {id} reason {why} dist {dist:.1} m height {:+.1} m (window {} m, facing mapping {:?})",
                    pp[1] - ep[1],
                    stealth_window_m(),
                    self.facing.best()
                ));
                true
            }
            Some((since, last)) => {
                if reason.is_some() {
                    *last = now;
                }
                let over = if old_rule {
                    now.duration_since(*last) >= Duration::from_secs(1) && dist > 6.5
                } else {
                    now.duration_since(*last) >= STEALTH_LINGER
                };
                if over {
                    log::line(&format!("throw: re-engaged {id} after {} ms", now.duration_since(*since).as_millis()));
                    self.handoff.remove(&id);
                    false
                } else {
                    true
                }
            }
        }
    }

    /// Native session (both roles): enemies we approach for a stealth / plunging
    /// deathblow are claimed through the game's own ownership candidates (type 22,
    /// nsession.rs `set_stealth_claims`), so they run on our AI like offline.
    fn stealth_claims(&mut self, player: Chr, others: &[[f32; 3]]) {
        let now = Instant::now();
        if !crate::nsession::claims_wanted() || stealth_window_m() <= 0.0 {
            if !self.claims.is_empty() {
                self.claims.clear();
                crate::nsession::set_stealth_claims(Vec::new());
            }
            return;
        }
        // Rate limit: the walk below reads every known chr's position (3 ReadProcessMemory
        // each), so run it every 100 ms, not every frame. CLAIM_LINGER is 3 s, so the
        // claim timing is unchanged.
        if self.claims_at.is_some_and(|t| now.duration_since(t) < Duration::from_millis(100)) {
            return;
        }
        self.claims_at = Some(now);
        let Some(pp) = player.position() else { return };
        let reach = stealth_window_m() + STEALTH_HOLD_EXTRA_M;
        let near: Vec<(i32, Chr)> = self
            .known
            .iter()
            .filter(|(_, c)| c.position().is_some_and(|p| game::distance(pp, p) <= reach + PLUNGE_MAX_ABOVE_M))
            .copied()
            .collect();
        let mut changed = false;
        let gauge = game::boss_state().map(|b| b.entity);
        for (id, chr) in near {
            if chr.hp().is_none_or(|hp| hp <= 0) {
                continue;
            }
            // Never a boss (v8.6): the claim moved Genichiro to the friend's AI while the
            // host still ran it ("claimed by both machines" for 21 s, live 2026-10-25), so
            // two AIs drove it and it jumped between the players. Bosses take no stealth
            // deathblow in a fight anyway.
            // v8.49: nor a miniboss (two or more lives): Headless 1700500 was claimed "behind"
            // while the friend fought it, and both machines ran it (live 2026-10-27).
            if gauge == Some(id) || crate::bosses::encounter_of(id).is_some() || boss_like_lives(chr) {
                continue;
            }
            // Nor one another living player is fighting (v8.8, 20 m since v8.10): the claim handed an enemy
            // fighting the host to our AI, which knew nothing of the fight, so it "forgot"
            // the players and gave a stealth deathblow (live 2026-10-25). An unaware enemy
            // nobody stands next to is still ours to sneak up on.
            if chr.position().is_some_and(|ep| fought_by_another(ep, others)) {
                continue;
            }
            let holding = self.claims.contains_key(&id);
            let Some((why, dy)) = self.stealth_approach(chr, player, holding) else { continue };
            let Some(net_id) = netai::net_id(chr) else { continue };
            match self.claims.get_mut(&id) {
                Some(c) => c.1 = now,
                None => {
                    self.claims.insert(id, (net_id, now, why, false));
                    changed = true;
                    if self.claim_logs < 40 {
                        self.claim_logs += 1;
                        log::line(&format!(
                            "stealth: claiming enemy {id} (netId {net_id:#x}, {why}, height {dy:+.1} m) through the game's ownership list"
                        ));
                    }
                }
            }
        }
        let mut dropped = Vec::new();
        for (&id, c) in self.claims.iter_mut() {
            if now.duration_since(c.1) >= CLAIM_LINGER {
                dropped.push(id);
                continue;
            }
            // Evidence: the claim moved the enemy to our own AI (slot 37 picks the
            // ComManipulator for an enemy owned here).
            if !c.3
                && let Some(chr) = game::chr_by_entity_id(id)
                && netai::active_kind(chr) == 1
            {
                c.3 = true;
                if self.claim_logs < 80 {
                    self.claim_logs += 1;
                    log::line(&format!("stealth: enemy {id} runs on our own AI now (owned here after the claim)"));
                }
            }
        }
        for id in dropped {
            if let Some((_, _, _, local)) = self.claims.remove(&id)
                && self.claim_logs < 80
            {
                self.claim_logs += 1;
                log::line(&format!("stealth: claim on enemy {id} dropped (approach over; it was {}owned here)", if local { "" } else { "never " }));
            }
            changed = true;
        }
        if changed {
            crate::nsession::set_stealth_claims(self.claims.values().map(|c| c.0).collect());
        }
    }
}

/// "A throw is running on this chr", as `0xB4E9C0` gates on it: the ThrowNode
/// `*(ThrowModule+0x10)` AND its timer `node+0x144 > 0` (`0x802A60`). The node
/// alone stays allocated on some enemies: live 2026-10-23 four enemies 50-130 m
/// away were handed to their own AI by it and never came back (8-15 m desync).
pub(crate) fn throw_active(chr: Chr) -> bool {
    let Some(tm) = game::read_ptr(chr.0 + 0x1FF8).and_then(|m| game::read_ptr(m + 0x88)) else { return false };
    game::read_ptr(tm + 0x10)
        .filter(|&node| node != 0)
        .and_then(|node| game::read::<f32>(node + 0x144))
        .is_some_and(|t| t > 0.0)
}

impl Enemies {
    /// Client: entity ids of every scanned character that is dead here.
    pub fn dead_here_ids(&self) -> Vec<i32> {
        self.known
            .iter()
            .filter_map(|(id, _)| game::chr_by_entity_id(*id).filter(|c| c.hp().is_some_and(|hp| hp <= 0)).map(|_| *id))
            .collect()
    }

    /// Host, after its death reload: kill what the friend had killed (the
    /// reload brought everything back). Through the event command ForceDead
    /// (`0x751F20(entityId, noReward)`, EMEVD 2004[4]) without the reward: HP 0
    /// paid the sen out again (live 2026-10-23). Returns (killed, other).
    pub fn kill_ids(&self, ids: &[i32]) -> (usize, usize) {
        let mut killed = 0;
        for &id in ids {
            // Paid once per enemy per rest cycle (persist.rs): a kill of ours from before
            // the death reload pays nothing again, a friend's kill we never got does.
            if self.force_dead_one(id, !crate::persist::rewarded(id)) {
                killed += 1;
            }
        }
        (killed, ids.len() - killed)
    }

    /// livesync.rs / persist.rs: the friend's copy of `id` died (or it died earlier this
    /// session). ForceDead without the game's reward (`0x751F20(id, 1)`, like `kill_ids`);
    /// `loot` = we were not paid for this enemy yet in this rest cycle, so `loot_step`
    /// gives our own drops / sen once ("died without our kill"); otherwise it is marked
    /// no-loot. True = it was alive.
    pub fn force_dead_one(&self, id: i32, loot: bool) -> bool {
        // v8.25c: a kill order within 90 s of a wipe is from the old world (no kill, no loot).
        if crate::persist::wipe_recent(crate::persist::WIPE_STALE) {
            return false;
        }
        let force_dead: extern "C" fn(u32, u8) = unsafe { std::mem::transmute(game::module_base() + 0x751F20) };
        let Some(c) = game::chr_by_entity_id(id) else { return false };
        if c.hp().is_none_or(|hp| hp <= 0) {
            return false;
        }
        force_dead(id as u32, 1);
        if loot {
            self.no_loot.borrow_mut().remove(&id);
            self.force_loot.borrow_mut().insert(id);
        } else {
            self.no_loot.borrow_mut().insert(id);
        }
        true
    }
}

/// Settings for loot_step (settings file: share_loot, share_exp).
pub static SHARE_LOOT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
pub static SHARE_EXP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

impl Enemies {
    /// Each player gets their own drops, as in Elden Ring Seamless
    /// (SPEC-ITEM-CLEANUP.md §7): an enemy that dies in our world without
    /// going through the game's own death rewards (the friend's hit, our HP
    /// mirror) is given them here. The game's own path sets `chr+0x1A12` bit 1
    /// (drops `0x9FFF50`) and bit 2 (sen + experience `0x9FFD50`); bit 0x10 is
    /// "no drop". Our own kills already have the bits, so this only fills in.
    /// Only enemies we saw alive during this load, 1 s after death (the game's
    /// own path runs first), at 4 Hz.
    fn loot_step(&mut self, is_host: bool) {
        let now = Instant::now();
        if self.loot_at.is_some_and(|t| now.duration_since(t) < Duration::from_millis(250)) {
            return;
        }
        self.loot_at = Some(now);
        if !SHARE_LOOT.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        // Native session: the client has no host list (`latest`), its own scan stands in.
        let own_scan = is_host || native_owns_enemies();
        let ids: Vec<i32> = if own_scan { self.known.iter().map(|(id, _)| *id).collect() } else { self.latest.keys().copied().collect() };
        let base = game::module_base();
        for id in ids {
            if self.looted.contains(&id) {
                continue;
            }
            let Some(chr) = game::chr_by_entity_id(id) else { continue };
            let Some(hp) = chr.hp() else { continue };
            if hp > 0 {
                self.seen_alive.insert(id);
                self.dead_since.remove(&id);
                continue;
            }
            let forced = self.force_loot.borrow().contains(&id);
            if !self.seen_alive.contains(&id) && !forced {
                continue;
            }
            let since = *self.dead_since.entry(id).or_insert(now);
            if now.duration_since(since) < Duration::from_secs(1) {
                continue;
            }
            self.looted.insert(id);
            // Resurrect lock before / after every enemy death (resurrect.rs, log only).
            crate::resurrect::note_enemy_death(id, game::read::<u8>(chr.0 + 0x1A12).is_some_and(|b| b & 3 == 3));
            if self.no_loot.borrow().contains(&id) {
                continue;
            }
            let Some(mut bits) = game::read::<u8>(chr.0 + 0x1A12) else { continue };
            // The game paid this death itself (our own kill): count it for this rest cycle.
            if bits & 3 == 3 {
                crate::persist::mark_rewarded(id);
            }
            if bits & 0x10 != 0 {
                if forced && crate::persist::rewarded(id) {
                    log::line(&format!("loot: {id} already paid this rest cycle, nothing more"));
                    continue;
                }
                if !forced {
                    // A no-reward death that is not ours to pay (a script's kill).
                    log::line(&format!("loot: {id} skipped, no-drop bit set (bits {bits:#04x})"));
                    continue;
                }
                // Our ForceDead(noReward = 1) for a kill we were not paid for: clear the
                // bit so the game's own drop / sen functions pay out (they check only it).
                if !game::write(chr.0 + 0x1A12, bits & !0x10) {
                    continue;
                }
                bits &= !0x10;
            }
            let mut gave = Vec::new();
            if bits & 1 == 0 {
                let drops: extern "C" fn(usize) = unsafe { std::mem::transmute(base + 0x9FFF50) };
                drops(chr.0);
                gave.push("drops");
            }
            if bits & 2 == 0 && SHARE_EXP.load(std::sync::atomic::Ordering::Relaxed) {
                let souls: extern "C" fn(usize) = unsafe { std::mem::transmute(base + 0x9FFD50) };
                souls(chr.0);
                gave.push("sen+xp");
            }
            if !gave.is_empty() {
                crate::persist::mark_rewarded(id);
                log::line(&format!(
                    "loot: {id} died without our kill, gave {} (bits {bits:#04x}{})",
                    gave.join(" + "),
                    if forced { ", after our ForceDead" } else { "" }
                ));
            }
        }
    }
}

impl Enemies {
    /// The enemies this side tracks right now (host: the scan; client: what
    /// the host sends), resolved to their live characters.
    pub fn loaded_enemies(&self, is_host: bool) -> Vec<(i32, Chr)> {
        // Native session: the client has no host list (`latest`), its own scan stands in.
        let own_scan = is_host || native_owns_enemies();
        let ids: Vec<i32> = if own_scan { self.known.iter().map(|(id, _)| *id).collect() } else { self.latest.keys().copied().collect() };
        ids.into_iter().filter_map(|id| game::chr_by_entity_id(id).map(|c| (id, c))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_stealth_claim_on_an_enemy_another_player_fights() {
        let e = [10.0, 0.0, 10.0];
        assert!(fought_by_another(e, &[[0.0; 3], [14.0, 0.0, 14.0]]));
        assert!(fought_by_another(e, &[[25.0, 0.0, 10.0]]));
        assert!(!fought_by_another(e, &[[0.0, 0.0, 40.0], [40.0, 0.0, 10.0]]));
        assert!(!fought_by_another(e, &[]));
    }

    #[test]
    fn bosses_are_not_handed_to_the_friends_ai_for_a_stealth_approach() {
        assert!(!boss_handoff_allowed(true, "behind"));
        assert!(!boss_handoff_allowed(true, "above"));
        // The deathblow / throw reasons stay allowed on a boss, and non-bosses are unchanged.
        assert!(boss_handoff_allowed(true, "node"));
        assert!(boss_handoff_allowed(true, "posture"));
        assert!(boss_handoff_allowed(false, "behind"));
        assert!(boss_handoff_allowed(false, "above"));
    }

    fn chr_target(nid: u16) -> Vec<u8> {
        let mut rec = vec![0u8; NETAI_BEHAVIOR_LEN];
        netai::put_i32(&mut rec, netai::BEH_TARGET_MODE, netai::TARGET_CHR);
        netai::put_i32(&mut rec, netai::BEH_TARGET, nid as i32);
        rec
    }

    #[test]
    fn netai_targets_go_on_the_wire_as_enemy_or_player_slot() {
        let by_net: HashMap<u16, i32> = [(0x2005u16, 1100800)].into_iter().collect();
        let players = [(0xA000, PeerId(0)), (0xA001, PeerId(3)), (0xA002, PeerId(1))];
        let wire = |nid| {
            let mut r = chr_target(nid);
            let ok = target_to_wire(&mut r, &by_net, &players);
            (ok, netai::get_i32(&r, netai::BEH_TARGET_MODE), netai::get_i32(&r, netai::BEH_TARGET))
        };
        assert_eq!(wire(0x2005), (true, netai::WIRE_ENEMY, 1100800));
        // A client-targeted enemy names that client's slot: 0x10 + slot.
        assert_eq!(wire(0xA000), (true, 0x10, 0));
        assert_eq!(wire(0xA001), (true, 0x13, 0));
        assert_eq!(wire(0xA002), (true, 0x11, 0));
        // Unknown character: no target, counted as unmapped.
        assert_eq!(wire(0xA004), (false, netai::TARGET_NONE, 0xA004));
    }

    #[test]
    fn stealth_window_geometry() {
        // Enemy faces +z; the player at -z is behind it, at +z in front.
        let f = Some([0.0, 1.0]);
        assert_eq!(approach_kind([0.0, 0.0, -12.0], f, 15.0, false), Some("behind"));
        assert_eq!(approach_kind([0.0, 0.0, 12.0], f, 15.0, false), None);
        assert_eq!(approach_kind([0.0, 0.0, -16.0], f, 15.0, false), None);
        // Side-on (90 degrees) is not the rear 120 degrees, but holds once handed off.
        assert_eq!(approach_kind([8.0, 0.0, -0.5], f, 15.0, false), None);
        assert_eq!(approach_kind([8.0, 0.0, -0.5], f, 15.0, true), Some("behind"));
        assert_eq!(approach_kind([0.0, 0.0, -19.0], f, 15.0, true), Some("behind"));
        // From above (a roof, a branch): any direction, 2..25 m up, within 12 m.
        assert_eq!(approach_kind([3.0, 6.0, 3.0], f, 15.0, false), Some("above"));
        assert_eq!(approach_kind([0.0, 1.0, 3.0], f, 15.0, false), None);
        assert_eq!(approach_kind([13.0, 6.0, 0.0], f, 15.0, false), None);
        assert_eq!(approach_kind([0.0, 30.0, 0.0], f, 15.0, false), None);
        // Facing not learned yet: only the plunge test works.
        assert_eq!(approach_kind([0.0, 0.0, -3.0], None, 15.0, false), None);
        // Old rule (0): 5 m behind only, no plunge.
        assert_eq!(approach_kind([0.0, 0.0, -4.0], f, 0.0, false), Some("behind"));
        assert_eq!(approach_kind([0.0, 0.0, -6.0], f, 0.0, false), None);
        assert_eq!(approach_kind([0.0, 3.0, 2.0], f, 0.0, false), None);
    }
}
