# Gap analysis vs ERSC / DS3SC / Sekiro Online (2026-10-24)

Research pass (web + local docs RE-ERSC.md, WEB-SEAMLESS-DEATH-RULES.md, WEB-PRIOR-ART.md,
SEAMLESS-RESEARCH.md). Only what is still missing; nothing below is verified live.

## P1 (breaks a playthrough)
1. **Multi-phase / fake-death bosses** (Guardian Ape headless phase, Genichiro -> Isshin,
   True Corrupted Monk 3 lives, Folding Screen Monkeys, Divine Dragon): no handling in
   bossrules/persist. Risk: first "death" taken as the kill (rewards, spectate release,
   records). Fix: per-boss table of the real defeat flag; phase cutscene = fight running.
   ERSC 1.8.3 had the same class (Radahn spectators).
   https://steamcommunity.com/app/1245620/discussions/0/4406291166503639205/?l=english&ctp=14
2. **Story phases half-applied** (Interior Ministry invasion, burning castle, Shura branch):
   merge_quests is per NPC (§19); a phase spans many NPCs + map flags. Fix: phase flag sets as
   units in merge/mirror, doctor compares phases, in-game flag repair.
   https://ersc-docs.github.io/workarounds/
3. **Ending / NG+**: joiner could get the NG+ prompt or Temple idol option in the host's world;
   own-world restore would then write the old cycle over it. Fix: block next-cycle while
   connected, refuse cross-cycle joins unless an ini allows (doctor.rs:87 only warns).
   https://sekiro-shadows-die-twice.fandom.com/wiki/New_Game_Plus
4. **Dragonrot on NPCs**: 9510 is per player, per-NPC sick flags unidentified; if shared, one
   player's deaths sicken the other's NPCs (RE-DEATH-RESET.md:184 suspects). Fix: RE + policy.
5. **Doors, levers, lifts, breakables**: doors.rs never found a door. ERSC 1.7.7/1.7.8 and
   DS3SC 0.07 fixed the same class. https://www.nexusmods.com/darksouls3/mods/1895

## P2 (noticeable)
6. Cutscene state: pause expel/warps/boss gate, show "in cutscene", place the other player after.
7. Both players talking to the same NPC (Owl's Shura question, Divine Child): talk lock, option
   "quest choices only in the host's world".
8. Bell Demon / Kuro's Charm: not in doctor; enemy HP differs per machine. Use host's value.
   https://sekiroshadowsdietwice.wiki.fextralife.com/Bell+Demon
9. Version check order: lib.rs:96-101 installs icons::install_early before the v1.06 check.
10. Quest rewards for the joiner (§23/§33 untested): Healing Gourd, Mortal Blade, Emma's items,
    Sculptor's prosthetics, Divine Child rice, ending items.

## P3 (polish)
Death count / death messages on nameplate (DS3SC 0.1.0); boss music for the non-fighter;
OneDrive save-folder warning; leaving mid-boss as a live test case. Invasions: n/a in Sekiro.

## Top 10
1. Boss defeat-flag table (Ape, Isshin, True Monk, Owl, Monkeys, Divine Dragon, Demon of Hatred).
2. Doors -> levers, lifts, breakables.
3. NG+ block + cycle gate.
4. Story-phase flag groups in merge/mirror + doctor.
5. Per-NPC Dragonrot flags.
6. Cutscene state detection.
7. NPC talk lock / host-world-only quest choices.
8. In-game flag repair.
9. Bell Demon / Kuro's Charm in doctor, host value for scaling.
10. Exe version check before patching; live test pass for the quest-reward list.
