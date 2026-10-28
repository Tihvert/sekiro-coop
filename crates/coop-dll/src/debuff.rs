//! Death penalty in co-op (ER Seamless "death debuffs", SPEC-DEBUFF-SCALING.md
//! §2, phase 1): every real death while connected adds a stack (max 5) that
//! lowers the player's max HP and max posture by `debuff_pct` % each, until
//! the player rests at an idol themself. Applied by writing the player's base
//! values in the data module every frame (the game recomputes max from them);
//! PlayerGameData, i.e. the save, is never written. Stacks survive a restart
//! through `coop_state.json`.
//!
//! Phase 2 (§2.4, setting `death_debuff_icon`, default on): the penalty is our
//! own SpEffect row per stack instead (speffect.rs: icon, max HP / posture and
//! attack power rates), and the base value writes are skipped. If the rows
//! cannot be appended, phase 1 runs as before.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};

use crate::game::{module_base, read, read_ptr, write};
use crate::log;
use crate::speffect::Effects;

/// Current stack count, for the overlay / status line.
pub static STACKS: AtomicU8 = AtomicU8::new(0);

const GAME_DATA_MAN: usize = 0x3D5AAC0;
const WORLD_CHR_MAN: usize = 0x3D7A1E0;
const WCM_MAIN_PLAYER: usize = 0x88;
const DEATH_COMMITTED: usize = 0x3EA0D74;
const PGD_BASE_HP: usize = 0x20;
const PGD_BASE_POSTURE: usize = 0x3C;
const CHR_MODULES: usize = 0x1FF8;
const MOD_DATA: usize = 0x18;
const DATA_BASE_MAX_HP: usize = 0x138;
const DATA_BASE_MAX_POSTURE: usize = 0x150;
const DATA_HP: usize = 0x130;
pub const MAX_STACKS: u8 = 5;

#[derive(Default)]
pub struct Debuff {
    stacks: u8,
    loaded: bool,
    last_committed: u32,
    applied: Option<(i32, i32)>,
    effects: Effects,
}

fn state_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("sekiro-coop").join("coop_state.json")
}

impl Debuff {
    fn load(&mut self) {
        self.loaded = true;
        let text = std::fs::read_to_string(state_path()).unwrap_or_default();
        // {"death_stacks": N}
        if let Some(n) = text.split("\"death_stacks\"").nth(1).and_then(|r| {
            r.trim_start_matches([' ', ':']).chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u8>().ok()
        }) {
            self.stacks = n.min(MAX_STACKS);
            log::line(&format!("debuff: {} stacks from coop_state.json", self.stacks));
        }
    }

    fn save(&self) {
        let _ = std::fs::write(state_path(), format!("{{\"death_stacks\": {}}}\n", self.stacks));
    }

    /// Every in-world frame. `enabled`: setting on; `connected`: a friend is
    /// linked; `own_rest`: we rested ourselves this frame (not a mirrored rest).
    pub fn on_frame(&mut self, enabled: bool, connected: bool, own_rest: bool, pct: u32) {
        if !self.loaded {
            self.load();
        }
        let committed = read::<u32>(module_base() + DEATH_COMMITTED).unwrap_or(0);
        if committed == 1 && self.last_committed != 1 && enabled && connected && self.stacks < MAX_STACKS {
            self.stacks += 1;
            self.save();
            log::line(&format!("debuff: stacks {} (death)", self.stacks));
        }
        self.last_committed = committed;
        if own_rest && self.stacks > 0 {
            self.stacks = 0;
            self.save();
            log::line("debuff: cleared (own rest)");
        }
        STACKS.store(self.stacks, Relaxed);
        let k = if enabled { self.stacks as i32 } else { 0 };
        let icon = crate::tuning::DEATH_DEBUFF_ICON.load(Relaxed);
        if icon != 0 && self.effects.ready(pct, icon) {
            // Phase 2: undo an earlier phase 1 write once, then only the SpEffect.
            if self.applied.is_some() && self.apply(0, pct as i32) {
                self.applied = None;
            }
            self.effects.tick(k as u32);
        } else {
            self.effects.clear();
            // Only while there are stacks, plus one write that undoes them; with no
            // stacks the game's own base values are never touched.
            if k > 0 {
                self.apply(k, pct as i32);
            } else if self.applied.is_some() && self.apply(0, pct as i32) {
                self.applied = None;
            }
        }
    }

    /// Phase 1 base value write. False when the player or its data is not readable.
    fn apply(&mut self, k: i32, pct: i32) -> bool {
        let base = module_base();
        let Some(pgd) = read_ptr(base + GAME_DATA_MAN).and_then(|g| read_ptr(g + 0x08)) else { return false };
        let Some(player) = read_ptr(base + WORLD_CHR_MAN).and_then(|w| read_ptr(w + WCM_MAIN_PLAYER)) else { return false };
        let Some(data) = read_ptr(player + CHR_MODULES).and_then(|m| read_ptr(m + MOD_DATA)) else { return false };
        if read::<i32>(data + DATA_HP).is_none_or(|hp| hp <= 0) {
            return false;
        }
        let (Some(hp0), Some(po0)) = (read::<i32>(pgd + PGD_BASE_HP), read::<i32>(pgd + PGD_BASE_POSTURE)) else {
            return false;
        };
        if hp0 <= 0 || po0 <= 0 {
            return false;
        }
        let keep = (100 - pct.clamp(0, 19) * k).max(5);
        let want = ((hp0 * keep / 100).max(1), (po0 * keep / 100).max(1));
        let cur = (read::<i32>(data + DATA_BASE_MAX_HP), read::<i32>(data + DATA_BASE_MAX_POSTURE));
        if cur.0 != Some(want.0) {
            write(data + DATA_BASE_MAX_HP, want.0);
        }
        if cur.1 != Some(want.1) {
            write(data + DATA_BASE_MAX_POSTURE, want.1);
        }
        if self.applied != Some(want) {
            self.applied = Some(want);
            log::line(&format!("debuff: base hp {hp0}->{} posture {po0}->{} ({k} stacks)", want.0, want.1));
        }
        true
    }
}
