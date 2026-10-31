//! Enemy and boss HP / posture scaling with the number of players (ER
//! Seamless defaults: enemies HP +35 %, posture +15 %; bosses +100 % / +20 %
//! per extra player). SPEC-DEBUFF-SCALING.md §3, option (a′): each loaded
//! enemy's base values in its data module (`+0x138` max HP, `+0x150` max
//! posture) are set to NpcParam × factor; the game recomputes max from them
//! every frame and heals by the growth, so a full enemy stays full. Both
//! machines do this with the HOST's percentages (sent on connect), so max HP
//! matches and the client's HP mirror never clamps.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::game::{Chr, read, read_ptr, write};
use crate::log;

const CHR_PARAM_REF: usize = 0x2010;
const REF_ROW: usize = 0x08;
const NPC_HP: usize = 0x20;
const NPC_POSTURE: usize = 0xF0;
/// i8 deathblow count: 2 or more = boss or miniboss.
const NPC_DEATHBLOWS: usize = 0x126;
const CHR_MODULES: usize = 0x1FF8;
const MOD_DATA: usize = 0x18;
const DATA_HP: usize = 0x130;
const DATA_BASE_MAX_HP: usize = 0x138;
const DATA_BASE_MAX_POSTURE: usize = 0x150;
/// Current / max posture in the data module (posture.rs, livesync.rs): F11 diagnostics only.
const DATA_POSTURE: usize = 0x148;
const DATA_MAX_POSTURE: usize = 0x14C;
/// Enemies in the F11 dump.
const DUMP_ROWS: usize = 12;

/// Percent added per extra player, and the players counted (the host's values on every
/// machine; PLAN-MULTIPLAYER P4: the host decides the count, 1 + connected peers).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScaleCfg {
    pub enemy_hp: u16,
    pub enemy_posture: u16,
    pub boss_hp: u16,
    pub boss_posture: u16,
    /// Players the host counts (2 with two players: v7.5's fixed "linked = 2").
    pub players: u8,
    /// The host's `death_debuffs` (v8.31: the host decides it for everyone). `None` = an old
    /// body without it: the receiver keeps its own setting.
    pub debuffs: Option<bool>,
}

impl ScaleCfg {
    /// 8 bytes of percentages, the player count, then the death-debuff flag (10th byte).
    pub fn encode(&self) -> Vec<u8> {
        let mut out: Vec<u8> =
            [self.enemy_hp, self.enemy_posture, self.boss_hp, self.boss_posture].iter().flat_map(|v| v.to_le_bytes()).collect();
        out.push(self.players);
        if let Some(d) = self.debuffs {
            out.push(d as u8);
        }
        out
    }

    /// A body without the count (an 8-byte config) counts two players, as it meant before;
    /// a 9-byte body has no debuff flag (`None`).
    pub fn decode(b: &[u8]) -> Option<Self> {
        let v = |i: usize| b.get(i * 2..i * 2 + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
        let players = b.get(8).copied().unwrap_or(2).clamp(1, crate::peers::MAX_PLAYERS as u8);
        let debuffs = b.get(9).map(|&x| x != 0);
        Some(Self { enemy_hp: v(0)?, enemy_posture: v(1)?, boss_hp: v(2)?, boss_posture: v(3)?, players, debuffs })
    }
}

/// Host: players the scaling counts. `session_players` = 1 + connected peers; `in_block` =
/// peers in our map block; `mode` `tuning::SCALING_COUNT_*` (session by default).
pub fn players_for(session_players: u32, in_block: u32, mode: u8) -> u32 {
    if mode == crate::tuning::SCALING_COUNT_BLOCK { 1 + in_block } else { session_players.max(1) }
}

/// Each further extra player adds this share (per mille) of the one before it (v8.8): the
/// second player adds the full percentage (the tested two-player numbers), the third
/// `decay`, the fourth `decay`^2 ... Posture decays faster than HP: in Sekiro posture is the
/// fight (the deathblow), and more players already fill it from more sides, so a big
/// posture bar punishes the deflecting the game is about (owner + four AI reviews
/// 2026-10-25: all put six-player posture far under the old linear 5x).
pub const HP_DECAY: i64 = 850;
pub const POSTURE_DECAY: i64 = 600;

/// Extra-player weight in per mille: 1000 for the first extra player, then `decay` of the
/// previous one each.
fn weight(extra: u32, decay: i64) -> i64 {
    let (mut w, mut sum) = (1000i64, 0i64);
    for _ in 0..extra {
        sum += w;
        w = w * decay / 1000;
    }
    sum
}

/// The scaled base value: `base * (1 + pct/100 * weight(players - 1))`.
pub fn scaled(base: i32, pct: i32, players: u32, decay: i64) -> i32 {
    let w = weight(players.saturating_sub(1), decay);
    (base as i64 * (100_000 + pct as i64 * w) / 100_000) as i32
}

/// The chr is a boss or miniboss by its NpcParam (2+ deathblows), as `apply` decides it.
pub fn npc_boss(chr: Chr) -> bool {
    read_ptr(chr.0 + CHR_PARAM_REF)
        .and_then(|r| read_ptr(r + REF_ROW))
        .and_then(|row| read::<i8>(row + NPC_DEATHBLOWS))
        .is_some_and(|n| n >= 2)
}

/// Adds the `extra` chrs that `list` does not have yet (same chr or same entity).
///
/// Live 1.0.5 (a host's log): miniboss 1100300 was missing from the enemy scan list after
/// death reloads, so it stayed at NpcParam HP on one machine (8522 / posture 3213) while the
/// other had it scaled (12780 / 4820): the audit saw HP and lives differ for minutes and the
/// fight looked out of sync. Bosses now come from the game's own chr list every second too.
pub fn add_missing(list: &mut Vec<(i32, Chr)>, extra: impl IntoIterator<Item = (i32, Chr)>) {
    for (entity, chr) in extra {
        if !list.iter().any(|&(e, c)| e == entity || c == chr) {
            list.push((entity, chr));
        }
    }
}

/// What the last `apply` used and wrote, for the F11 dump (read-only mirror).
struct Mirror {
    cfg: Option<ScaleCfg>,
    players: u32,
    bosses: Vec<i32>,
    written: HashMap<(usize, i32), (i32, i32)>,
}

static MIRROR: Mutex<Option<Mirror>> = Mutex::new(None);

#[derive(Default)]
pub struct Scaling {
    /// What we wrote last per (chr address, entity): (hp, posture).
    written: HashMap<(usize, i32), (i32, i32)>,
    skip_logged: u32,
}

impl Scaling {
    /// A load rebuilt every chr: forget what we wrote.
    pub fn reset(&mut self) {
        self.written.clear();
        if let Ok(mut m) = MIRROR.lock() {
            *m = None;
        }
    }

    /// Sets each living enemy's base values for `players` players. With one
    /// player (or `cfg` None) the base goes back to the NpcParam value.
    pub fn apply(&mut self, enemies: impl Iterator<Item = (i32, Chr)>, players: u32, cfg: Option<ScaleCfg>, bosses: &[i32]) {
        self.apply_inner(enemies, players, cfg, bosses);
        if let Ok(mut m) = MIRROR.try_lock() {
            *m = Some(Mirror { cfg, players, bosses: bosses.to_vec(), written: self.written.clone() });
        }
    }

    fn apply_inner(&mut self, enemies: impl Iterator<Item = (i32, Chr)>, players: u32, cfg: Option<ScaleCfg>, bosses: &[i32]) {
        for (entity, chr) in enemies {
            let Some(row) = read_ptr(chr.0 + CHR_PARAM_REF).and_then(|r| read_ptr(r + REF_ROW)) else { continue };
            let Some(data) = read_ptr(chr.0 + CHR_MODULES).and_then(|m| read_ptr(m + MOD_DATA)) else { continue };
            if read::<i32>(data + DATA_HP).is_none_or(|hp| hp <= 0) {
                continue;
            }
            let (Some(npc_hp), Some(npc_po)) = (read::<i32>(row + NPC_HP), read::<u16>(row + NPC_POSTURE).map(i32::from)) else {
                continue;
            };
            if npc_hp <= 0 {
                continue;
            }
            let boss = read::<i8>(row + NPC_DEATHBLOWS).is_some_and(|n| n >= 2) || bosses.contains(&entity);
            let (p_hp, p_po) = match cfg {
                Some(c) if boss => (c.boss_hp as i32, c.boss_posture as i32),
                Some(c) => (c.enemy_hp as i32, c.enemy_posture as i32),
                None => (0, 0),
            };
            let want = (scaled(npc_hp, p_hp, players, HP_DECAY), scaled(npc_po, p_po, players, POSTURE_DECAY));
            let key = (chr.0, entity);
            let last = self.written.get(&key).copied();
            let cur = (read::<i32>(data + DATA_BASE_MAX_HP), read::<i32>(data + DATA_BASE_MAX_POSTURE));
            let ours_hp = cur.0 == Some(npc_hp) || last.is_some_and(|l| cur.0 == Some(l.0));
            let ours_po = cur.1 == Some(npc_po) || last.is_some_and(|l| cur.1 == Some(l.1));
            if cur.0 != Some(want.0) {
                if ours_hp {
                    write(data + DATA_BASE_MAX_HP, want.0);
                } else if self.skip_logged < 20 {
                    self.skip_logged += 1;
                    log::line(&format!("scale: skip {entity} base {:?} != npc {npc_hp} (something else changed it)", cur.0));
                }
            }
            if cur.1 != Some(want.1) && ours_po && npc_po > 0 {
                write(data + DATA_BASE_MAX_POSTURE, want.1);
            }
            if last != Some(want) {
                self.written.insert(key, want);
            }
        }
    }
}

/// One enemy's scaling facts for the F11 dump.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub entity: i32,
    pub boss: bool,
    pub dist: f32,
    pub npc: (i32, i32),
    pub base: (Option<i32>, Option<i32>),
    pub want: (i32, i32),
    pub ours: (bool, bool),
    pub hp: Option<i32>,
    pub posture: Option<i32>,
    pub max_posture: Option<i32>,
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "?".to_string(), |x| x.to_string())
}

/// The `scale: F11 ...` line for one row. `want` differing from `base` means the scaled
/// value is not in the data module (yet); `ours` false means our write is refused.
pub fn format_row(r: &Row, extra: &str) -> String {
    format!(
        "scale: F11 enemy {} boss={} dist={:.0}m npc hp/po {}/{} base hp/po {}/{} want hp/po {}/{} ours hp/po {}/{} hp={} posture={} max_posture={}{extra}",
        r.entity,
        r.boss,
        r.dist,
        r.npc.0,
        r.npc.1,
        opt(r.base.0),
        opt(r.base.1),
        r.want.0,
        r.want.1,
        r.ours.0,
        r.ours.1,
        opt(r.hp),
        opt(r.posture),
        opt(r.max_posture)
    )
}

/// The `n` nearest items by distance (ties keep the input order).
pub fn nearest_n<T>(mut v: Vec<(f32, T)>, n: usize) -> Vec<(f32, T)> {
    v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    v.truncate(n);
    v
}

/// One enemy's row; `None` when its param or data module cannot be read or it is dead.
fn row_of(entity: i32, chr: Chr, dist: f32, m: Option<&Mirror>) -> Option<Row> {
    let row = read_ptr(chr.0 + CHR_PARAM_REF).and_then(|r| read_ptr(r + REF_ROW))?;
    let data = read_ptr(chr.0 + CHR_MODULES).and_then(|m| read_ptr(m + MOD_DATA))?;
    let hp = read::<i32>(data + DATA_HP);
    if hp.is_none_or(|h| h <= 0) {
        return None;
    }
    let npc = (read::<i32>(row + NPC_HP)?, i32::from(read::<u16>(row + NPC_POSTURE)?));
    let boss = read::<i8>(row + NPC_DEATHBLOWS).is_some_and(|n| n >= 2) || m.is_some_and(|m| m.bosses.contains(&entity));
    let (p_hp, p_po) = match m.and_then(|m| m.cfg) {
        Some(c) if boss => (i32::from(c.boss_hp), i32::from(c.boss_posture)),
        Some(c) => (i32::from(c.enemy_hp), i32::from(c.enemy_posture)),
        None => (0, 0),
    };
    let players = m.map_or(1, |m| m.players);
    let want = (scaled(npc.0, p_hp, players, HP_DECAY), scaled(npc.1, p_po, players, POSTURE_DECAY));
    let base = (read::<i32>(data + DATA_BASE_MAX_HP), read::<i32>(data + DATA_BASE_MAX_POSTURE));
    let last = m.and_then(|m| m.written.get(&(chr.0, entity)).copied());
    let ours = (
        base.0 == Some(npc.0) || last.is_some_and(|l| base.0 == Some(l.0)),
        base.1 == Some(npc.1) || last.is_some_and(|l| base.1 == Some(l.1)),
    );
    Some(Row {
        entity,
        boss,
        dist,
        npc,
        base,
        want,
        ours,
        hp,
        posture: read::<i32>(data + DATA_POSTURE),
        max_posture: read::<i32>(data + DATA_MAX_POSTURE),
    })
}

/// F11 dump lines (`scale: F11 ...`): the active config, then up to `DUMP_ROWS` living
/// enemies nearest to `ppos`. `mark` = the spectate lock point: the enemy nearest to it
/// within 6 m is logged as the locked target. `tag_of` adds net-owner text. Read-only.
pub fn snapshot_lines(enemies: Vec<(i32, Chr)>, ppos: Option<[f32; 3]>, mark: Option<[f32; 3]>, tag_of: impl Fn(Chr) -> String) -> Vec<String> {
    let guard = MIRROR.try_lock().ok();
    let m = guard.as_ref().and_then(|g| g.as_ref());
    let mut out = Vec::new();
    match m {
        Some(m) => out.push(format!("scale: F11 cfg {:?} players={} bosses={:?} enemies={}", m.cfg, m.players, m.bosses, enemies.len())),
        None => out.push(format!("scale: F11 cfg none yet (apply has not run) enemies={}", enemies.len())),
    }
    let dist_to = |c: Chr, p: Option<[f32; 3]>| match (c.position(), p) {
        (Some(a), Some(b)) => crate::game::distance(a, b),
        _ => 0.0,
    };
    match mark {
        Some(mk) => {
            let near = nearest_n(enemies.iter().map(|&(e, c)| (dist_to(c, Some(mk)), (e, c))).collect(), 1);
            match near.first().copied().filter(|(d, _)| *d <= 6.0) {
                Some((d, (e, c))) => {
                    let line = row_of(e, c, dist_to(c, ppos), m).map(|r| format_row(&r, &format!(" LOCKED(mark {d:.1}m){}", tag_of(c))));
                    out.push(line.unwrap_or_else(|| format!("scale: F11 LOCKED target entity {e}: unreadable or dead")));
                }
                None => out.push("scale: F11 lock mark set but no enemy within 6 m of it".to_string()),
            }
        }
        None => out.push("scale: F11 no spectate lock mark (the player's own lock-on target has no accessor)".to_string()),
    }
    let cands: Vec<(f32, (i32, Chr))> = enemies.into_iter().map(|(e, c)| (dist_to(c, ppos), (e, c))).collect();
    let mut n = 0;
    // Dead ones do not use the cap: look at more candidates, keep 12 living.
    for (d, (e, c)) in nearest_n(cands, DUMP_ROWS * 4) {
        if n >= DUMP_ROWS {
            break;
        }
        if let Some(r) = row_of(e, c, d, m) {
            out.push(format_row(&r, &tag_of(c)));
            n += 1;
        }
    }
    out
}

#[cfg(test)]
mod diag_tests {
    use super::*;

    #[test]
    fn nearest_n_orders_and_caps() {
        let v = vec![(5.0, 'a'), (1.0, 'b'), (3.0, 'c'), (1.0, 'd')];
        assert_eq!(nearest_n(v.clone(), 3), vec![(1.0, 'b'), (1.0, 'd'), (3.0, 'c')]);
        assert_eq!(nearest_n(v, 99).len(), 4);
        assert!(nearest_n(Vec::<(f32, u8)>::new(), 5).is_empty());
        assert_eq!(nearest_n(vec![(f32::NAN, 1), (2.0, 2)], 2).len(), 2);
    }

    #[test]
    fn format_row_shows_every_field_and_unknowns() {
        let r = Row {
            entity: 1020000,
            boss: true,
            dist: 12.4,
            npc: (1000, 100),
            base: (Some(1000), None),
            want: (1600, 160),
            ours: (true, false),
            hp: Some(900),
            posture: None,
            max_posture: Some(100),
        };
        assert_eq!(
            format_row(&r, " x"),
            "scale: F11 enemy 1020000 boss=true dist=12m npc hp/po 1000/100 base hp/po 1000/? want hp/po 1600/160 ours hp/po true/false hp=900 posture=? max_posture=100 x"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_bosses_are_added_once() {
        let mut list = vec![(1100100, Chr(0x10)), (1100200, Chr(0x20))];
        add_missing(&mut list, [(1100300, Chr(0x30)), (1100100, Chr(0x10)), (1100200, Chr(0x99)), (1100400, Chr(0x20))]);
        assert_eq!(list, vec![(1100100, Chr(0x10)), (1100200, Chr(0x20)), (1100300, Chr(0x30))]);
        add_missing(&mut list, [(1100300, Chr(0x30))]);
        assert_eq!(list.len(), 3);
        let mut empty = Vec::new();
        add_missing(&mut empty, [(1100300, Chr(0x30))]);
        assert_eq!(empty, vec![(1100300, Chr(0x30))]);
    }

    #[test]
    fn config_round_trip_with_the_player_count() {
        let c = ScaleCfg { enemy_hp: 60, enemy_posture: 150, boss_hp: 60, boss_posture: 80, players: 4, debuffs: None };
        assert_eq!(ScaleCfg::decode(&c.encode()), Some(c));
        assert_eq!(c.encode().len(), 9);
        // 10 bytes: the host's death-debuff flag rides along; an old decoder ignores the byte.
        for flag in [true, false] {
            let c10 = ScaleCfg { debuffs: Some(flag), ..c };
            let body = c10.encode();
            assert_eq!(body.len(), 10);
            assert_eq!(ScaleCfg::decode(&body), Some(c10));
            assert_eq!(ScaleCfg::decode(&body[..9]).map(|d| d.debuffs), Some(None));
        }
        // An 8-byte body (no count) means two players.
        let old = &c.encode()[..8];
        assert_eq!(ScaleCfg::decode(old).map(|d| d.players), Some(2));
        assert_eq!(ScaleCfg::decode(&[1, 2, 3]), None);
    }

    /// P4: the host counts 1 + connected peers (session) or 1 + peers in its block; each
    /// extra player adds the percentage once. Two players: the v7.5 numbers.
    #[test]
    fn players_and_multiplier_per_extra_player() {
        use crate::tuning::{SCALING_COUNT_BLOCK, SCALING_COUNT_SESSION};
        for n in 1..=6u32 {
            assert_eq!(players_for(n, 0, SCALING_COUNT_SESSION), n);
        }
        assert_eq!(players_for(5, 2, SCALING_COUNT_BLOCK), 3);
        assert_eq!(players_for(5, 0, SCALING_COUNT_BLOCK), 1);
        // Defaults 60 / 150 / 60 / 80: two players as before (both curves).
        assert_eq!(scaled(1000, 60, 2, HP_DECAY), 1600);
        assert_eq!(scaled(100, 150, 2, HP_DECAY), 250);
        assert_eq!(scaled(100, 80, 2, POSTURE_DECAY), 180);
        assert_eq!(scaled(1000, 60, 1, HP_DECAY), 1000);
        // Three to six players: each step smaller, posture smaller still.
        assert_eq!(scaled(1000, 60, 3, HP_DECAY), 2110);
        assert_eq!(scaled(1000, 150, 6, HP_DECAY), 6559);
        assert_eq!(scaled(1000, 80, 3, POSTURE_DECAY), 2280);
        assert_eq!(scaled(1000, 80, 6, POSTURE_DECAY), 2844);
        // 1000 = the old linear rule.
        assert_eq!(scaled(100, 150, 6, 1000), 850);
    }
}
