# Multi-phase bosses: table + spec (2026-10-24, research pass, NOT verified live)

Sources: thefifthmatt SoulsRandomizers Sekiro data (decompiled EMEVD)
https://raw.githubusercontent.com/thefifthmatt/SoulsRandomizers/master/dists/Base/enemy.txt ,
.../events.txt ; SoulSplitter boss flags
https://raw.githubusercontent.com/FrankvdStam/SoulSplitter/main/src/SoulMemory/Sekiro/Boss.cs ;
our logs (log.old.txt, friend logs, boss_start_flags.txt).

## Finding
EMEVD calls HandleBossDefeat (HBD, `0x689260`) once per boss after the LAST phase, with the
encounter's main entity (not the chr that died). Exception: Emma fires an HBD before Isshin
Ashina in the same encounter. Risks are our fallbacks that treat "phase entity dead/gone" as
the kill: the gauge-off miniboss branch (`dead_here` -> EV_BOSS(2), bossrules ~:720),
`recent_gauge` / `mini_recent` excluding a dead `last_gauge`, and spectate's
`boss_dead = ... is_none_or(h<=0)` (~:1030).

Butterfly [VERIFIED]: event 11005812 sees 1000800 at 0 bars, disables it, enables 1000810,
moves the gauge (`gauge now entity 1000810`; flags 11005641, 11005802, 11005812). Final
deathblow on 1000810 sets 11000800, 5.33 s later HBD(1000800) + 9302.

## Table ([VERIFIED] from the EMEVD dump unless marked)
| Boss | Entities | HBD final | Defeat flag | Phase flags / notes |
|---|---|---|---|---|
| Lady Butterfly | 1000800 P1 -> 1000810 P2 | 1000800 | 9302 | 11005802, 11005812, 11005641; gauge switches entity |
| Gyoubu | 1100800 | 1100800 | 9301 | none |
| Genichiro (castle) | 1110800 -> 1110801 | 1110801 | 9303 | 11115803, 11115802 |
| Tutorial Genichiro | 1120800 | none | 11120801 | scripted loss (no spectate) |
| Great Shinobi Owl | 1110850 | 1110850 | 9308 | same entity, 2 phases |
| Emma -> Isshin Ashina | 1110900 Emma, 1110920 Isshin | 1110920 (intermediate HBD 1110900) | 9316 | 9315 = Emma, intermediate; 11110900 |
| Sword Saint Isshin | 1120830 Genichiro -> 1120860 Isshin | 1120860 | 9312 | 9311, 6811, 11120830 at Genichiro's end, no HBD; gauge off gap [ARV] |
| Corrupted Monk | 1500800 | 1500800 | 9306 | none |
| Guardian Ape | 1700800 (+ helper 1700801) | 1700800 (after headless phase) | 9304 | NEVER use 11700800 (may be set at the fake death) [ARV] |
| Headless Ape | 1700850 + 1700852 (2nd ape) + 1700851 | 1700850 | 9307 | two gauges [ARV] |
| Folding Screen Monkeys | 2000801-2000804, gauge holder 2000800 | 2000800 | 9305 | monkey "deaths" = captures |
| Owl (Father) | 1000900, 1000901 | 1000900 | 9317 | none |
| Demon of Hatred | 1100900 | 1100900 | 9313 | 3 phases, same entity |
| True Corrupted Monk | 2500850 (+ illusions 2500851/853/854) | 2500850 | 9309 | 3 lives |
| Divine Dragon | 2500800 | 2500800 | 9310 | scripted |
Minibosses (Lone Shadows, Snake Eyes, Long-arm Centipede, Headless, Shichimen) have no HBD:
the v7.10 mini path stays for them.

Second issue [ARV]: Gyoubu fight, the friend's AwardItemLot(2000) (Memory) ran ~6 s before
the friend's own HBD and was classed "script lot": our synced 9301 / 11100800 started the
reward event first. Reward classification keyed only on HBD time is fragile.

## Implementation
New `crates/coop-dll/src/bosses.rs`:
```rust
pub struct Encounter { pub name: &'static str, pub entities: &'static [i32], pub final_entities: &'static [i32],
  pub hbd_final: i32, pub hbd_intermediate: &'static [i32], pub defeat_flag: u32,
  pub phase_flags: &'static [u32], pub scripted_loss: bool }
pub static ENCOUNTERS: &[Encounter] = &[ /* rows above; Emma+Isshin = ONE encounter */ ];
pub fn encounter_of(entity: i32) -> Option<&'static Encounter>; // any member incl. helpers
pub enum Sig { Hbd(i32), GaugeOff { entity: i32, chr_dead_or_gone: bool }, GaugeSwitch { from: i32, to: i32 }, FlagOn(u32) }
pub enum Verdict { Defeated(i32), PhaseEnd, FightGoesOn, MiniDefeated, Retreat, Unknown }
pub fn classify(sig: Sig, flag_on: &dyn Fn(u32) -> bool) -> Verdict;
```
classify:
- Hbd(e): Defeated if e == hbd_final (or boss not in table); e in hbd_intermediate -> PhaseEnd.
- FlagOn(defeat_flag) -> Defeated; FlagOn(phase flag) -> PhaseEnd; other -> Unknown.
- GaugeOff, table entity: flag_on(defeat_flag) -> Defeated; else entity not in final_entities
  -> PhaseEnd; else chr dead -> FightGoesOn (HBD follows within ~7 s); else Retreat.
- GaugeOff, non-table entity: today's logic (dead -> MiniDefeated, else Retreat).
- GaugeSwitch inside one encounter -> FightGoesOn; gauge entity -1 = same fight.

bossrules.rs:
- Defeat detour: PhaseEnd -> log `boss: phase end E (intermediate HBD)`, no EV_BOSS(2), no
  release, no note_defeat, clear DEFEAT_AT_MS.
- Gauge off: EV_BOSS(2) only for Defeated / MiniDefeated, EV_BOSS(3) only for Retreat; on
  PhaseEnd / FightGoesOn keep gauge_seen_at = now (fight held 10 s or until the next gauge).
- recent_gauge, mini_recent and spectate's boss_dead: for table entities test "encounter
  defeated" (flag_on(defeat_flag) or HBD of hbd_final seen) instead of chr hp <= 0.
- Store wait_encounter instead of wait_boss for the release.
- scripted_loss (1120800): SPEC_WANTED = false.
- Our own 93xx defeat flag turning on (synced or not) counts as Defeated (friend's kill
  arriving by flag sync before our HBD).
keyitems.rs: boss_window also true for 15 s after the encounter's defeat_flag turns on here.
persist.rs boss_step: non-final member dies -> remove only its record; encounter defeated ->
remove all members' records; restore_plan never restores a phase-1 entity once a later member
has a record.

Tests in bosses.rs: Hbd(1000800) -> Defeated; GaugeSwitch 1000800->1000810 -> FightGoesOn;
GaugeOff{1120830,dead}, 9312 off -> PhaseEnd; FlagOn(9311) -> PhaseEnd; Hbd(1110900) -> PhaseEnd,
Hbd(1110920) -> Defeated; GaugeOff{1110800,gone} -> PhaseEnd; GaugeOff{1700800,dead} 9304 off
-> FightGoesOn, on -> Defeated; FlagOn(11700800) -> Unknown; GaugeOff{2000802,dead} -> PhaseEnd;
non-table GaugeOff dead -> MiniDefeated / alive -> Retreat; encounter_of(1700852) ==
encounter_of(1700850); gauge entity -1 -> FightGoesOn.

[LIVE] still: Guardian Ape gauge + 11700800 at the fake death; Sword Saint gauge-off gap;
Monkeys gauge entity; Headless Ape gauge entities.
