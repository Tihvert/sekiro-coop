//! Pure row model of the native "Co-op Settings" options page (v8.34b, T1 of
//! docs/DESIGN-NATIVE-OPTIONS-PAGE.md). No game memory, no FFI: the later glue (optmenu.rs)
//! asks this module what a row shows, whether it can change, and which ini key / text a
//! chosen value becomes. Every row is a combo row whose list has at most 32 items.
#![allow(dead_code)]

use crate::menu::Role;

/// Capacity of the game's combo item list (`0x140CFF3C0` refuses more).
pub const MAX_ITEMS: usize = 32;
/// Rows the page may hold at most.
pub const MAX_ROWS: usize = 48;

pub const PCT_MAX_GRID: i64 = 300;
pub const PCT_STEP: i64 = 10;
/// The ini accepts 0..=1000 for the scaling keys.
pub const PCT_INI_MAX: i64 = 1000;
pub const SCALE_MIN: i64 = 50;
pub const SCALE_MAX: i64 = 200;
pub const SCALE_STEP: i64 = 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowKind {
    /// 0..=300 step 10 (31 items) + one slot for an off-grid current value.
    Pct,
    OnOff,
    /// Fixed labels; the value is the index.
    Choice(&'static [&'static str]),
    /// 50..=200 step 10 (+ an off-grid slot).
    Scale,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Authority {
    /// Edited by the host; a client sees the host's value as a 1-item list.
    Host,
    /// Gameplay rule read per machine: editable only when not connected; 1-item list in a session.
    HostOffline,
    /// Per player, live.
    Local,
}

#[derive(Clone, Copy, Debug)]
pub struct RowDef {
    pub id: u16,
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: RowKind,
    pub authority: Authority,
    /// For a Choice row whose ini value is a word: the words by index (empty = the ini holds the index number).
    pub ini_names: &'static [&'static str],
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Item {
    pub value_index: u8,
    pub label: String,
}

use Authority::{Host, HostOffline, Local};
use RowKind::{Choice, OnOff, Pct, Scale};

const fn r(
    id: u16,
    key: &'static str,
    label: &'static str,
    help: &'static str,
    kind: RowKind,
    authority: Authority,
) -> RowDef {
    RowDef { id, key, label, help, kind, authority, ini_names: &[] }
}

const fn rn(
    id: u16,
    key: &'static str,
    label: &'static str,
    help: &'static str,
    kind: RowKind,
    authority: Authority,
    ini_names: &'static [&'static str],
) -> RowDef {
    RowDef { id, key, label, help, kind, authority, ini_names }
}

const MERGE: &[&str] = &["Off", "Merge", "Coarse"];
const OVERHEAD: &[&str] = &["Name and ping", "Name only", "Nothing"];
const SPEC_CAM: &[&str] = &["Death screen only", "Follow, no death screen", "Follow behind death screen"];
const SPEC_ORBIT: &[&str] = &["Fixed behind player", "Game camera input", "Right stick"];
const HP_POS: &[&str] = &["Above head", "Left list", "Top list"];
const HP_CONTENT: &[&str] = &["Name and bar", "Bar only", "Bar and numbers"];
const PAD_BTN: &[&str] = &["Back", "Start", "L3", "R3", "None"];

static ROWS: [RowDef; 40] = [
    r(1, "enemy_health_scaling", "Enemy health", "Extra enemy health per added player. Host sets it.", Pct, Host),
    r(2, "enemy_posture_scaling", "Enemy posture", "Extra enemy posture per added player. Host sets it.", Pct, Host),
    r(3, "boss_health_scaling", "Boss health", "Extra boss health per added player. Host sets it.", Pct, Host),
    r(4, "boss_posture_scaling", "Boss posture", "Extra boss posture per added player. Host sets it.", Pct, Host),
    r(5, "death_debuffs", "Death debuff", "Each co-op death lowers max HP and posture by 10 %.", OnOff, Host),
    r(6, "share_loot", "Share loot", "Both players get drops from enemies the friend killed.", OnOff, HostOffline),
    r(7, "share_exp", "Share sen and XP", "Both players get experience and sen from kills.", OnOff, HostOffline),
    r(8, "share_key_items", "Share key items", "Story items given to one player are given to both.", OnOff, HostOffline),
    r(9, "share_resurrect_unlock", "Share Resurrection", "A kill by any player unlocks the next Resurrection.", OnOff, HostOffline),
    r(10, "boss_spectate", "Boss spectating", "After you die in a boss fight, wait and watch the friend.", OnOff, HostOffline),
    r(11, "boss_reward", "Boss rewards for both", "The friend also receives the boss's item rewards.", OnOff, HostOffline),
    r(12, "death_refill", "Refill after death", "After a death reload you wake with full HP, gourd and items.", OnOff, HostOffline),
    r(13, "join_rest", "Rest when joining", "Joining rests you at the arrival idol (enemies respawn).", OnOff, HostOffline),
    r(14, "boss_start_gate", "Boss start gate", "A boss fight far away waits until you come near.", OnOff, HostOffline),
    r(15, "boss_persist_on_death", "Boss keeps damage", "A boss keeps lost health when one player dies and reloads.", OnOff, HostOffline),
    r(16, "merge_quests", "Quest progress", "Joiner: keep NPC quest progress from the host's world.", Choice(MERGE), HostOffline),
    r(17, "quest_choices_host_only", "Host decides quests", "Only the host can talk to quest NPCs in a joined world.", OnOff, HostOffline),
    r(18, "shared_npc_intros", "Shared NPC intros", "NPC introductions are shared at once instead of per player.", OnOff, HostOffline),
    r(19, "sync_posture", "Sync posture", "The friend's hits also fill the host's enemy posture bars.", OnOff, HostOffline),
    r(20, "block_quit_in_coop", "Block quit in co-op", "Quit Game is cancelled while a companion is connected.", OnOff, HostOffline),
    r(21, "block_ng_in_coop", "Block New Game+ in co-op", "New Game+ is refused while a companion is connected.", OnOff, HostOffline),
    r(22, "no_pause_in_coop", "No pause in co-op", "Menus do not pause the game while connected.", OnOff, HostOffline),
    r(23, "allow_cross_cycle", "Allow different cycles", "Allow co-op between players on different NG cycles.", OnOff, HostOffline),
    r(24, "party_hp_bars", "Party health bars", "Show the other players' health bars.", OnOff, Local),
    rn(25, "party_hp_position", "Health bar position", "Where the other players' health bars are drawn.", Choice(HP_POS), Local, &["head", "left", "top"]),
    rn(26, "party_hp_content", "Health bar shows", "What the other players' health bars show.", Choice(HP_CONTENT), Local, &["name_hp", "hp", "hp_numbers"]),
    r(27, "party_hp_scale", "Health bar size", "Size of the health bars in percent.", Scale, Local),
    r(28, "show_status_overlay", "Status line", "Show the co-op status line in the top left corner.", OnOff, Local),
    r(29, "overhead_player_display", "Name above player", "What is shown above the friend's head.", Choice(OVERHEAD), Local),
    r(30, "spectate_camera", "Spectator camera", "What you see while waiting after a death in a boss fight.", Choice(SPEC_CAM), Local),
    r(31, "spectate_orbit", "Spectator steering", "How you steer the spectator camera.", Choice(SPEC_ORBIT), Local),
    r(32, "spectate_lock", "Spectator lock-on", "Lock-on while spectating.", OnOff, Local),
    r(33, "auto_follow", "Follow idol warps", "Follow the friend's idol warps automatically.", OnOff, Local),
    rn(34, "f11_pad_button", "Pad menu button", "Controller button that writes the debug MARK line.", Choice(PAD_BTN), Local, &["back", "start", "l3", "r3", "none"]),
    r(35, "dialog_default_yes", "Yes preselected", "In co-op yes/no boxes, Yes is highlighted first.", OnOff, Local),
    r(36, "confirm_items", "Confirm co-op items", "Co-op items ask first in a yes/no box.", OnOff, Local),
    r(37, "custom_icons", "Custom icons", "Own pictures for co-op items. Needs a restart.", OnOff, Local),
    r(38, "native_notices", "Game-style notices", "Show some co-op messages as the game's own message.", OnOff, Local),
    r(39, "fps_unlock", "Unlock 120 fps", "Frame rate unlocked to 120 fps.", OnOff, Local),
    r(40, "vsync", "VSync", "Off is needed for 120 fps on a 60 Hz screen.", OnOff, Local),
];

/// The game's option list does not scroll past 11 rows (live test 2026-10-26: the selection leaves the view at row 11), so the page shows the first 11.
pub const PAGE_ROWS: usize = 10;

/// The rows the page builds.
pub fn page_rows() -> &'static [RowDef] {
    &ROWS[..PAGE_ROWS]
}

/// All rows in page order.
pub fn rows() -> &'static [RowDef] {
    &ROWS
}

/// Whether the value of this row can be changed on this machine right now.
pub fn can_change(row: &RowDef, role: Role, connected: bool) -> bool {
    match row.authority {
        Authority::Local => true,
        Authority::Host => crate::menu::can_edit(role),
        Authority::HostOffline => crate::menu::can_edit(role) && !connected,
    }
}

/// The value brought into the row's legal range (the ini's ranges).
pub fn clamp_for(row: &RowDef, value: i64) -> i64 {
    match row.kind {
        Pct => value.clamp(0, PCT_INI_MAX),
        OnOff => value.clamp(0, 1),
        Choice(l) => value.clamp(0, l.len() as i64 - 1),
        Scale => value.clamp(SCALE_MIN, SCALE_MAX),
    }
}

fn on_grid(row: &RowDef, v: i64) -> bool {
    match row.kind {
        Pct => (0..=PCT_MAX_GRID).contains(&v) && v % PCT_STEP == 0,
        Scale => (SCALE_MIN..=SCALE_MAX).contains(&v) && v % SCALE_STEP == 0,
        _ => true,
    }
}

fn grid_len(row: &RowDef) -> usize {
    match row.kind {
        Pct => (PCT_MAX_GRID / PCT_STEP + 1) as usize,
        OnOff => 2,
        Choice(l) => l.len(),
        Scale => ((SCALE_MAX - SCALE_MIN) / SCALE_STEP + 1) as usize,
    }
}

fn grid_value(row: &RowDef, i: usize) -> i64 {
    match row.kind {
        Pct => i as i64 * PCT_STEP,
        Scale => SCALE_MIN + i as i64 * SCALE_STEP,
        _ => i as i64,
    }
}

/// Label of one value of this row.
pub fn label_of(row: &RowDef, v: i64) -> String {
    match row.kind {
        Pct => format!("{v} %"),
        Scale => format!("{v} %"),
        OnOff => (if v != 0 { "On" } else { "Off" }).to_owned(),
        Choice(l) => l[clamp_for(row, v) as usize].to_owned(),
    }
}

/// The list shown for the row. Locked rows get exactly one item, the current value.
pub fn items(row: &RowDef, current: i64, role: Role, connected: bool) -> Vec<Item> {
    let cur = clamp_for(row, current);
    if !can_change(row, role, connected) {
        return vec![Item { value_index: 0, label: label_of(row, cur) }];
    }
    let n = grid_len(row);
    let mut v: Vec<Item> =
        (0..n).map(|i| Item { value_index: i as u8, label: label_of(row, grid_value(row, i)) }).collect();
    if !on_grid(row, cur) {
        v.push(Item { value_index: n as u8, label: label_of(row, cur) });
    }
    v
}

/// Index of the current value inside `items` (0 for a locked row).
pub fn index_of(row: &RowDef, current: i64, role: Role, connected: bool) -> u8 {
    if !can_change(row, role, connected) {
        return 0;
    }
    let cur = clamp_for(row, current);
    if !on_grid(row, cur) {
        return grid_len(row) as u8;
    }
    match row.kind {
        Pct => (cur / PCT_STEP) as u8,
        Scale => ((cur - SCALE_MIN) / SCALE_STEP) as u8,
        _ => cur as u8,
    }
}

/// The value a chosen list index stands for. `None` when the row is locked or the index is invalid.
pub fn value_at(row: &RowDef, index: u8, current: i64, role: Role, connected: bool) -> Option<i64> {
    if !can_change(row, role, connected) {
        return None;
    }
    let i = index as usize;
    let n = grid_len(row);
    if i < n {
        return Some(grid_value(row, i));
    }
    let cur = clamp_for(row, current);
    (i == n && !on_grid(row, cur)).then_some(cur)
}

/// The `(key, value)` pair `settings::write_keys` / `rewrite_ini` takes.
pub fn ini_update(row: &RowDef, value: i64) -> (&'static str, String) {
    let v = clamp_for(row, value);
    let text = match row.ini_names.get(v as usize) {
        Some(name) if matches!(row.kind, Choice(_)) => (*name).to_owned(),
        _ => v.to_string(),
    };
    (row.key, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROLES: [Role; 3] = [Role::Solo, Role::Host, Role::Client];

    fn row(key: &str) -> &'static RowDef {
        rows().iter().find(|r| r.key == key).unwrap()
    }

    #[test]
    fn ids_unique_and_labels_filled() {
        let mut ids: Vec<u16> = rows().iter().map(|r| r.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), rows().len());
        let mut keys: Vec<&str> = rows().iter().map(|r| r.key).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), rows().len());
        for r in rows() {
            assert!(!r.label.is_empty() && r.label.len() <= 40, "{}", r.key);
            assert!(!r.help.is_empty() && r.help.len() <= 90, "{} help {}", r.key, r.help.len());
        }
    }

    #[test]
    fn keys_exist_in_default_ini_template() {
        // settings::TEMPLATE is private: read the template out of the settings.rs source.
        let src = include_str!("settings.rs");
        for r in rows() {
            let needle = format!("\n{} = ", r.key);
            assert!(src.contains(&needle), "{} not in the default ini template", r.key);
        }
    }

    #[test]
    fn caps_hold_for_all_rows_and_roles() {
        assert!(rows().len() <= MAX_ROWS);
        for r in rows() {
            for role in ROLES {
                for connected in [false, true] {
                    for cur in [-5, 0, 5, 60, 65, 300, 305, 1000, 5000] {
                        let it = items(r, cur, role, connected);
                        assert!(!it.is_empty() && it.len() <= MAX_ITEMS, "{} {}", r.key, it.len());
                        assert!(it.iter().all(|i| !i.label.is_empty()));
                        let idx = index_of(r, cur, role, connected);
                        assert!((idx as usize) < it.len(), "{} idx {idx}", r.key);
                    }
                }
            }
        }
    }

    #[test]
    fn pct_grid_and_off_grid_slot() {
        let r = row("enemy_health_scaling");
        let it = items(r, 60, Role::Host, false);
        assert_eq!(it.len(), 31);
        assert_eq!(it[0].label, "0 %");
        assert_eq!(it[6].label, "60 %");
        assert_eq!(it[30].label, "300 %");
        assert_eq!(index_of(r, 60, Role::Host, false), 6);
        let it = items(r, 65, Role::Host, false);
        assert_eq!(it.len(), 32);
        assert_eq!(it[31].label, "65 %");
        assert_eq!(index_of(r, 65, Role::Host, false), 31);
        assert_eq!(value_at(r, 31, 65, Role::Host, false), Some(65));
        assert_eq!(value_at(r, 31, 60, Role::Host, false), None);
        assert_eq!(value_at(r, 32, 65, Role::Host, false), None);
        // above the grid but inside the ini range
        assert_eq!(items(r, 450, Role::Solo, false)[31].label, "450 %");
        assert_eq!(value_at(r, 31, 450, Role::Solo, false), Some(450));
        assert_eq!(value_at(r, 31, 5000, Role::Solo, false), Some(1000));
    }

    #[test]
    fn scale_row() {
        let r = row("party_hp_scale");
        let it = items(r, 100, Role::Client, true);
        assert_eq!(it.len(), 16);
        assert_eq!(it[0].label, "50 %");
        assert_eq!(it[15].label, "200 %");
        assert_eq!(index_of(r, 100, Role::Client, true), 5);
        assert_eq!(value_at(r, 5, 100, Role::Client, true), Some(100));
        assert_eq!(items(r, 55, Role::Solo, false).len(), 17);
        assert_eq!(value_at(r, 16, 55, Role::Solo, false), Some(55));
        assert_eq!(clamp_for(r, 10), 50);
        assert_eq!(clamp_for(r, 999), 200);
    }

    #[test]
    fn client_sees_host_rows_locked() {
        for r in rows().iter().filter(|r| r.authority != Local) {
            let it = items(r, if matches!(r.kind, Pct) { 60 } else { 1 }, Role::Client, false);
            assert_eq!(it.len(), 1, "{}", r.key);
            assert_eq!(index_of(r, 1, Role::Client, false), 0);
            assert!(!can_change(r, Role::Client, false));
            assert_eq!(value_at(r, 0, 1, Role::Client, false), None);
        }
        assert_eq!(items(row("enemy_health_scaling"), 60, Role::Client, true)[0].label, "60 %");
        assert_eq!(items(row("death_debuffs"), 1, Role::Client, true)[0].label, "On");
    }

    #[test]
    fn host_offline_rows_lock_when_connected() {
        let r = row("share_loot");
        assert!(can_change(r, Role::Solo, false));
        assert!(!can_change(r, Role::Solo, true));
        assert!(!can_change(r, Role::Host, true));
        assert!(can_change(r, Role::Host, false));
        assert_eq!(items(r, 1, Role::Host, true).len(), 1);
        assert_eq!(items(r, 1, Role::Host, false).len(), 2);
        // host rows stay editable for a connected host
        assert!(can_change(row("boss_health_scaling"), Role::Host, true));
    }

    #[test]
    fn local_rows_always_editable() {
        for role in ROLES {
            for connected in [false, true] {
                let r = row("party_hp_bars");
                assert!(can_change(r, role, connected));
                assert_eq!(items(r, 1, role, connected).len(), 2);
            }
        }
    }

    #[test]
    fn value_round_trips() {
        for r in rows() {
            for role in ROLES {
                for connected in [false, true] {
                    for cur in [0, 1, 2, 60, 65, 100, 250] {
                        if !can_change(r, role, connected) {
                            continue;
                        }
                        let c = clamp_for(r, cur);
                        let idx = index_of(r, cur, role, connected);
                        assert_eq!(value_at(r, idx, cur, role, connected), Some(c), "{} cur {cur}", r.key);
                        for it in items(r, cur, role, connected) {
                            let v = value_at(r, it.value_index, cur, role, connected).unwrap();
                            assert_eq!(label_of(r, v), it.label);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn ini_updates_have_the_ini_format() {
        assert_eq!(ini_update(row("death_debuffs"), 1), ("death_debuffs", "1".to_owned()));
        assert_eq!(ini_update(row("share_loot"), 0), ("share_loot", "0".to_owned()));
        assert_eq!(ini_update(row("boss_health_scaling"), 70), ("boss_health_scaling", "70".to_owned()));
        assert_eq!(ini_update(row("boss_health_scaling"), 5000), ("boss_health_scaling", "1000".to_owned()));
        assert_eq!(ini_update(row("party_hp_position"), 2), ("party_hp_position", "top".to_owned()));
        assert_eq!(ini_update(row("party_hp_content"), 2), ("party_hp_content", "hp_numbers".to_owned()));
        assert_eq!(ini_update(row("f11_pad_button"), 4), ("f11_pad_button", "none".to_owned()));
        assert_eq!(ini_update(row("merge_quests"), 2), ("merge_quests", "2".to_owned()));
        assert_eq!(ini_update(row("party_hp_scale"), 10), ("party_hp_scale", "50".to_owned()));
        assert_eq!(ini_update(row("spectate_camera"), 9), ("spectate_camera", "2".to_owned()));
    }

    #[test]
    fn ini_names_match_choice_labels() {
        for r in rows() {
            if let Choice(l) = r.kind {
                assert!(r.ini_names.is_empty() || r.ini_names.len() == l.len(), "{}", r.key);
            }
        }
    }

    #[test]
    fn rewrite_ini_takes_our_updates() {
        let text = "[GAMEPLAY]\nshare_loot = 1\nboss_health_scaling = 50 ; c\n[DISPLAY]\nparty_hp_position = head\n";
        let upd = [
            ini_update(row("share_loot"), 0),
            ini_update(row("boss_health_scaling"), 80),
            ini_update(row("party_hp_position"), 1),
        ];
        let out = crate::settings::rewrite_ini(text, &upd);
        assert_eq!(
            out,
            "[GAMEPLAY]\nshare_loot = 0\nboss_health_scaling = 80 ; c\n[DISPLAY]\nparty_hp_position = left\n"
        );
    }

    #[test]
    fn menu_values_keys_match_our_rows() {
        let v = crate::menu::Values::default();
        for (k, s) in crate::menu::ini_updates(v) {
            let r = row(k);
            let cur: i64 = s.parse().unwrap();
            assert_eq!(ini_update(r, cur), (k, s));
        }
    }
}
