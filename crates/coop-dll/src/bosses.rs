//! Multi-phase boss table and the classification of "a boss entity went away" signals
//! (docs/SPEC-BOSS-PHASES.md, 2026-10-24, research pass).
//!
//! EMEVD calls HandleBossDefeat (HBD, `0x689260`) once per boss after the LAST phase, with
//! the encounter's main entity (not the chr that died). Exception: Emma fires an HBD before
//! Isshin Ashina in the same encounter. Our fallbacks that treated "the phase entity is dead /
//! gone" as the kill (gauge off, `recent_gauge`, spectate's `boss_dead`) ask `classify` /
//! `encounter_of` here instead. Pure, unit tested. Minibosses are not in the table: they keep
//! the v7.10 mini path (no HBD; dead at gauge off = defeated).
//!
//! Marks: [VERIFIED] = from the EMEVD dump (thefifthmatt SoulsRandomizers, SoulSplitter);
//! [ARV] = research only, no live log yet; [LIVE] still open: Guardian Ape gauge + 11700800
//! at the fake death, Sword Saint gauge-off gap, Monkeys gauge entity, Headless Ape gauges.

/// One boss fight (all its phases / helpers).
pub struct Encounter {
    pub name: &'static str,
    /// Every entity of the fight, phase order, helpers included.
    pub entities: &'static [i32],
    /// Entities that can still be the fight's last (their going away may be the kill).
    pub final_entities: &'static [i32],
    /// The entity the last HBD names (0 = none).
    pub hbd_final: i32,
    /// Entities whose HBD is only a phase end (Emma).
    pub hbd_intermediate: &'static [i32],
    pub defeat_flag: u32,
    pub phase_flags: &'static [u32],
    /// Scripted loss (tutorial Genichiro): no spectating.
    pub scripted_loss: bool,
}

pub static ENCOUNTERS: &[Encounter] = &[
    // [VERIFIED] 1000800 -> 1000810 (event 11005812: gauge moves to 1000810); final deathblow
    // on 1000810 sets 11000800, 5.33 s later HBD(1000800) + 9302.
    Encounter {
        name: "Lady Butterfly",
        entities: &[1000800, 1000810],
        final_entities: &[1000810],
        hbd_final: 1000800,
        hbd_intermediate: &[],
        defeat_flag: 9302,
        phase_flags: &[11005802, 11005812, 11005641],
        scripted_loss: false,
    },
    // [VERIFIED]
    Encounter {
        name: "Gyoubu",
        entities: &[1100800],
        final_entities: &[1100800],
        hbd_final: 1100800,
        hbd_intermediate: &[],
        defeat_flag: 9301,
        phase_flags: &[],
        scripted_loss: false,
    },
    // [VERIFIED]
    Encounter {
        name: "Genichiro (castle)",
        entities: &[1110800, 1110801],
        final_entities: &[1110801],
        hbd_final: 1110801,
        hbd_intermediate: &[],
        defeat_flag: 9303,
        phase_flags: &[11115803, 11115802],
        scripted_loss: false,
    },
    // [VERIFIED] scripted loss, no HBD.
    Encounter {
        name: "Tutorial Genichiro",
        entities: &[1120800],
        final_entities: &[1120800],
        hbd_final: 0,
        hbd_intermediate: &[],
        defeat_flag: 11120801,
        phase_flags: &[],
        scripted_loss: true,
    },
    // [VERIFIED] same entity, 2 phases.
    Encounter {
        name: "Great Shinobi Owl",
        entities: &[1110850],
        final_entities: &[1110850],
        hbd_final: 1110850,
        hbd_intermediate: &[],
        defeat_flag: 9308,
        phase_flags: &[],
        scripted_loss: false,
    },
    // [VERIFIED] ONE encounter: Emma's HBD (9315) is intermediate, Isshin Ashina's is final.
    Encounter {
        name: "Emma / Isshin Ashina",
        entities: &[1110900, 1110920],
        final_entities: &[1110920],
        hbd_final: 1110920,
        hbd_intermediate: &[1110900],
        defeat_flag: 9316,
        phase_flags: &[9315, 11110900],
        scripted_loss: false,
    },
    // [VERIFIED] Genichiro's end sets 9311, 6811, 11120830 without HBD; gauge-off gap [ARV].
    Encounter {
        name: "Sword Saint Isshin",
        entities: &[1120830, 1120860],
        final_entities: &[1120860],
        hbd_final: 1120860,
        hbd_intermediate: &[],
        defeat_flag: 9312,
        phase_flags: &[9311, 6811, 11120830],
        scripted_loss: false,
    },
    // [VERIFIED]
    Encounter {
        name: "Corrupted Monk",
        entities: &[1500800],
        final_entities: &[1500800],
        hbd_final: 1500800,
        hbd_intermediate: &[],
        defeat_flag: 9306,
        phase_flags: &[],
        scripted_loss: false,
    },
    // [VERIFIED] HBD after the headless phase. Never list 11700800: it may be set at the fake
    // death [ARV]. Helper 1700801.
    Encounter {
        name: "Guardian Ape",
        entities: &[1700800, 1700801],
        final_entities: &[1700800],
        hbd_final: 1700800,
        hbd_intermediate: &[],
        defeat_flag: 9304,
        phase_flags: &[],
        scripted_loss: false,
    },
    // [VERIFIED] entities, [ARV] two gauges (second ape 1700852).
    Encounter {
        name: "Headless Ape",
        entities: &[1700850, 1700852, 1700851],
        final_entities: &[1700850, 1700852, 1700851],
        hbd_final: 1700850,
        hbd_intermediate: &[],
        defeat_flag: 9307,
        phase_flags: &[],
        scripted_loss: false,
    },
    // [VERIFIED] monkey "deaths" are captures; the gauge holder 2000800 is the last entity [ARV].
    Encounter {
        name: "Folding Screen Monkeys",
        entities: &[2000801, 2000802, 2000803, 2000804, 2000800],
        final_entities: &[2000800],
        hbd_final: 2000800,
        hbd_intermediate: &[],
        defeat_flag: 9305,
        phase_flags: &[],
        scripted_loss: false,
    },
    // [VERIFIED]
    Encounter {
        name: "Owl (Father)",
        entities: &[1000900, 1000901],
        final_entities: &[1000900],
        hbd_final: 1000900,
        hbd_intermediate: &[],
        defeat_flag: 9317,
        phase_flags: &[],
        scripted_loss: false,
    },
    // [VERIFIED] 3 phases, same entity.
    Encounter {
        name: "Demon of Hatred",
        entities: &[1100900],
        final_entities: &[1100900],
        hbd_final: 1100900,
        hbd_intermediate: &[],
        defeat_flag: 9313,
        phase_flags: &[],
        scripted_loss: false,
    },
    // [VERIFIED] 3 lives, illusions 2500851/853/854.
    Encounter {
        name: "True Corrupted Monk",
        entities: &[2500850, 2500851, 2500853, 2500854],
        final_entities: &[2500850],
        hbd_final: 2500850,
        hbd_intermediate: &[],
        defeat_flag: 9309,
        phase_flags: &[],
        scripted_loss: false,
    },
    // [VERIFIED] scripted.
    Encounter {
        name: "Divine Dragon",
        entities: &[2500800],
        final_entities: &[2500800],
        hbd_final: 2500800,
        hbd_intermediate: &[],
        defeat_flag: 9310,
        phase_flags: &[],
        scripted_loss: false,
    },
];

/// The encounter `entity` belongs to (any member, helpers included).
pub fn encounter_of(entity: i32) -> Option<&'static Encounter> {
    if entity <= 0 {
        return None;
    }
    ENCOUNTERS.iter().find(|e| e.entities.contains(&entity))
}

/// Same table row.
pub fn same_encounter(a: &Encounter, b: &Encounter) -> bool {
    std::ptr::eq(a, b)
}

/// What the game showed us.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sig {
    /// HandleBossDefeat(entity).
    Hbd(i32),
    /// The boss gauge went away from `entity`.
    GaugeOff { entity: i32, chr_dead_or_gone: bool },
    /// The gauge moved from one entity to another (-1 = none known).
    GaugeSwitch { from: i32, to: i32 },
    /// An event flag turned on.
    FlagOn(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The fight is won (the entity the friend is told about).
    Defeated(i32),
    /// A phase is over, the fight goes on.
    PhaseEnd,
    /// Nothing ended (gauge moved, or the final phase's death awaits its HBD).
    FightGoesOn,
    /// A miniboss (not in the table) died.
    MiniDefeated,
    /// The boss is alive and its gauge left: a retreat.
    Retreat,
    Unknown,
}

/// `flag_on`: the live event flags (tests pass a closure).
pub fn classify(sig: Sig, flag_on: &dyn Fn(u32) -> bool) -> Verdict {
    match sig {
        Sig::Hbd(e) => match encounter_of(e) {
            Some(enc) if enc.hbd_intermediate.contains(&e) => Verdict::PhaseEnd,
            // The final entity, a boss not in the table, or another member of a table boss
            // (not expected): the old behaviour, a defeat.
            _ => Verdict::Defeated(e),
        },
        Sig::FlagOn(f) => {
            if let Some(enc) = ENCOUNTERS.iter().find(|e| e.defeat_flag == f) {
                Verdict::Defeated(final_entity(enc))
            } else if ENCOUNTERS.iter().any(|e| e.phase_flags.contains(&f)) {
                Verdict::PhaseEnd
            } else {
                Verdict::Unknown
            }
        }
        Sig::GaugeOff { entity, chr_dead_or_gone } => match encounter_of(entity) {
            None if chr_dead_or_gone => Verdict::MiniDefeated,
            None => Verdict::Retreat,
            Some(enc) if flag_on(enc.defeat_flag) => Verdict::Defeated(final_entity(enc)),
            Some(enc) if !enc.final_entities.contains(&entity) => Verdict::PhaseEnd,
            // The last phase died: its HBD follows within ~7 s.
            Some(_) if chr_dead_or_gone => Verdict::FightGoesOn,
            Some(_) => Verdict::Retreat,
        },
        Sig::GaugeSwitch { from, to } => {
            if from <= 0 || to <= 0 {
                return Verdict::FightGoesOn;
            }
            match (encounter_of(from), encounter_of(to)) {
                (Some(a), Some(b)) if same_encounter(a, b) => Verdict::FightGoesOn,
                _ => Verdict::Unknown,
            }
        }
    }
}

/// The entity a defeat of `enc` is reported with.
fn final_entity(enc: &Encounter) -> i32 {
    if enc.hbd_final > 0 { enc.hbd_final } else { enc.entities[0] }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: u32) -> bool {
        false
    }

    #[test]
    fn butterfly_hbd_defeats_and_the_gauge_switch_goes_on() {
        assert_eq!(classify(Sig::Hbd(1000800), &none), Verdict::Defeated(1000800));
        assert_eq!(classify(Sig::GaugeSwitch { from: 1000800, to: 1000810 }, &none), Verdict::FightGoesOn);
        // A gauge going to or from nothing known is the same fight.
        assert_eq!(classify(Sig::GaugeSwitch { from: 1000800, to: -1 }, &none), Verdict::FightGoesOn);
        assert_eq!(classify(Sig::GaugeSwitch { from: -1, to: 1000810 }, &none), Verdict::FightGoesOn);
    }

    #[test]
    fn sword_saint_genichiro_end_is_a_phase_end() {
        // Gauge off on Genichiro with 9312 off: phase end, not a kill.
        assert_eq!(
            classify(Sig::GaugeOff { entity: 1120830, chr_dead_or_gone: true }, &none),
            Verdict::PhaseEnd
        );
        assert_eq!(classify(Sig::FlagOn(9311), &none), Verdict::PhaseEnd);
        assert_eq!(classify(Sig::FlagOn(9312), &none), Verdict::Defeated(1120860));
    }

    #[test]
    fn emma_hbd_is_intermediate_isshin_final() {
        assert_eq!(classify(Sig::Hbd(1110900), &none), Verdict::PhaseEnd);
        assert_eq!(classify(Sig::Hbd(1110920), &none), Verdict::Defeated(1110920));
        assert_eq!(encounter_of(1110900).map(|e| e.name), encounter_of(1110920).map(|e| e.name));
    }

    #[test]
    fn genichiro_castle_phase_one_gone_is_a_phase_end() {
        assert_eq!(
            classify(Sig::GaugeOff { entity: 1110800, chr_dead_or_gone: true }, &none),
            Verdict::PhaseEnd
        );
    }

    #[test]
    fn guardian_ape_fake_death_waits_for_the_defeat_flag() {
        let off = |f: u32| f != 9304;
        let on = |_: u32| true;
        let sig = Sig::GaugeOff { entity: 1700800, chr_dead_or_gone: true };
        assert_eq!(classify(sig, &off), Verdict::FightGoesOn);
        assert_eq!(classify(sig, &on), Verdict::Defeated(1700800));
        // Alive and the gauge left: a retreat.
        assert_eq!(classify(Sig::GaugeOff { entity: 1700800, chr_dead_or_gone: false }, &off), Verdict::Retreat);
        // 11700800 may be set at the fake death: not a signal.
        assert_eq!(classify(Sig::FlagOn(11700800), &none), Verdict::Unknown);
    }

    #[test]
    fn monkeys_captures_are_phase_ends() {
        assert_eq!(
            classify(Sig::GaugeOff { entity: 2000802, chr_dead_or_gone: true }, &none),
            Verdict::PhaseEnd
        );
    }

    #[test]
    fn minibosses_keep_the_v710_path() {
        assert_eq!(
            classify(Sig::GaugeOff { entity: 1000999, chr_dead_or_gone: true }, &none),
            Verdict::MiniDefeated
        );
        assert_eq!(classify(Sig::GaugeOff { entity: 1000999, chr_dead_or_gone: false }, &none), Verdict::Retreat);
        assert_eq!(classify(Sig::Hbd(1000999), &none), Verdict::Defeated(1000999));
    }

    #[test]
    fn helpers_belong_to_their_encounter() {
        let a = encounter_of(1700852).unwrap();
        let b = encounter_of(1700850).unwrap();
        assert!(same_encounter(a, b));
        assert!(same_encounter(encounter_of(1700801).unwrap(), encounter_of(1700800).unwrap()));
        assert!(encounter_of(0).is_none() && encounter_of(-1).is_none());
    }

    #[test]
    fn the_table_is_consistent() {
        for e in ENCOUNTERS {
            assert!(!e.entities.is_empty(), "{}", e.name);
            assert!(e.final_entities.iter().all(|f| e.entities.contains(f)), "{}", e.name);
            assert!(e.hbd_intermediate.iter().all(|f| e.entities.contains(f)), "{}", e.name);
            for &m in e.entities {
                assert!(same_encounter(encounter_of(m).unwrap(), e), "{} entity {m} shared", e.name);
            }
        }
        assert!(encounter_of(1120800).is_some_and(|e| e.scripted_loss));
    }
}
