# Seamless co-op death, boss and world-state rules: what the FromSoft mods do (2026-10-24)

Research only, no code changes. Goal: align the Sekiro co-op mod's death / boss / rest rules with
the established seamless mods, and decide where Sekiro must differ.

This extends `SEAMLESS-RESEARCH.md`, `RE-ERSC.md`, `WEB-OPEN-SOURCE-COOP*.md` and
`SPEC-BOSS-RULES.md`. Facts already recorded there are not repeated unless a primary source
now confirms or corrects them.

**Method.** Nexus pages (description + full changelog) were read in a real browser (webcmd
Cloak profile), because raw fetches get 403. The ERSC docs site was fetched directly. The
ERSC 2.0.1 `ersc_settings.ini` and `locale/english.json` were read from the local extract
`F:\Temp\re_ersc\extract\SeamlessCoop\`. BBSC and the other Bloodborne port's docs were
downloaded raw from GitHub (copies in this session's scratchpad). Steam threads and press
articles were used only as secondary sources.

**Marks.** [V] = verified on a primary page (mod author's Nexus description/changelog, the mod's
own docs, settings or locale file, or the repo docs). [S] = secondary/community (press, Steam,
search snippets). [?] = uncertain or inferred. [ARV] = my inference from [V] facts.
Source ids (S1…) refer to the list at the end.

---

## 0. One-paragraph answer

All three LukeYui mods (ERSC, DS3SC, DSRSC) use the same rule text, word for word on Nexus.
- **Death outside a boss:** you respawn at your own last bonfire/grace in the same world. The
  session survives and the world does **not** reset.
- **Death during a boss fight or invasion:** "limbo", spectating others "until either everyone
  in the party has been defeated, or a player rests". The boss fight goes on without you, and you
  cannot rejoin it. Community sources add "or the boss is dead".
- **Resting:** one player resting resets the world for everyone and removes players from boss
  rooms.
- **ERSC extras:** it adds an optional stacking death debuff (Rot), cured only at grace. Its stated
  purpose is to stop one player dying over and over while the other keeps the world from resetting.
  ERSC also adds `always_spectate_on_death`.
- **Bloodborne:** both Bloodborne ports (BBSC and visy/bloodborne_pc_windows_port) copy this
  model. They document the hard edge cases: the boss gauge is cleared *before* the death step, the
  boss defeat flag arrives *after* the kill, a dead player's remote copy still looks alive, and a
  guest reloading on its own during a wipe breaks it.
- **Sekiro:** our mod keeps two worlds in sync instead of one native world. So the spectate
  deferral is load-bearing, not cosmetic. Sekiro's native Resurrect, its death penalty and
  deathblow phases also need explicit rules (§3).

---

## 1. Per-mod findings

### 1.1 Elden Ring Seamless Co-op (ERSC, LukeYui, Nexus 510, v2.0.1)

**Q1. Death outside a boss fight**
- "If a player dies, they will respawn in the same world at the last bonfire they rested at. The
  session will not be terminated." [V S1]
- The world does not reset when one player dies; the dead player comes back into the world the
  living left. The Rot text gives the reason it exists: "to avoid one player dying over and
  over without any downsides, while another player stays alive to keep the enemies from
  respawning." [V S1] So regular enemies are **not** reset for anyone by a single death [ARV, strong].
- Rot: optional (`death_debuffs`), "stackable debuff which will increase with subsequent
  deaths", five kinds picked at random, "You can only remove rot by sitting at a Site of Grace"
  [V S1, S3]. 1.3.1: "double the debuff potency per stack … maximum stacks of any rot is now 5
  (from 10)" [V S2]. 1.2.2: "You no longer have to be in a session to remove rot essence at a
  site of grace" [V S2].
- Respawn point of a joiner: 1.5.0 "The last bonfire that the host has rested at should now be
  synchronised to connecting clients" [V S2]. Until a joiner rests, they respawn at the
  host's last grace [ARV].
- `always_spectate_on_death` (added 1.9.5): "0 = Spectate only during boss fights and invasion
  encounters | 1 = Always spectate other players on death until party wipeout or bonfire rest"
  [V S3, S2]. The default is 0.
- Notifications: `"{} has died"`, death count on the nameplate (`overhead_player_display = 4`),
  and 1.9.6 "Death messages are now sent on death on starting spectate mode, rather than at the
  end" [V S4, S3, S2].

**Q2. Death inside a boss fight**
- "Dying during a boss battle or invasion will put your character into limbo, spectating other
  players until either everyone in the party has been defeated, or a player rests at a site of
  grace." [V S1] The Nexus text does not list "boss dies" as an exit. Community answers do:
  "If you die during a boss-fight, you enter spectate until boss is dead or everyone else dies"
  [S S9, S10]. Both are consistent: after the kill the boss bar is gone, so the boss-fight
  condition ends [ARV].
- The trigger is the boss health bar. 1.1.0: "Fixed a bug that would occasionally let players
  respawn even when a boss health bar is present" [V S2]. This matches RE-ERSC §3 (FeMan boss
  bar slots). Elden Ring Reforged's "always spectate" add-on works by showing a fake boss bar
  linked to your own HP, which "prevents players from respawning until a Grace is used" [S S11].
- Boss state: there is one world, so the boss keeps its HP and phase while you spectate. You do
  **not** re-enter the fight. You come back only after the fight ends (kill, wipe or rest) [V by rule
  text; ARV for HP].
- Spectator credit: 1.8.3 "Fixed an issue which would cause spectating players to only partially
  complete Starscourge Radahn" [V S2]. So spectators are meant to get the full kill.
  Community: both players get the rewards, but a spectator's dropped runes stay in the arena
  [S S12].
- Spectate fixes that reveal the design [V S2]:
  - 1.2.1 "enemies attack players who are spectating" (fixed).
  - 1.2.1 "break spectator mode if you activated the fall death camera" (fixed).
  - 1.4.3 "spectating players … leave behind a solid obstacle where they had died" (fixed).
  - 1.4.3 "camera will now swap between players based on death states instead of absolute HP".
  - 1.4.4 "use a player's health state instead of alive state to choose to spectate them"
    (fixed). Lesson: alive/dead must be an explicit state, not HP.
  - 1.4.3 "Enemy AI should now be more consistent, as player death states are updated faster".
- Scaling is fixed per fight. 1.2.5: "Fixed a bug that would cause substantial enemies to
  re-adjust their scaling parameters if a player or friendly NPC disconnected during the fight"
  [V S2].
- 1.7.4: "Disabled invasions during active boss fights" [V S2].

**Q3. Party wipe**
- The wipe ends limbo ("until everyone in the party has been defeated") [V S1]. Everyone then
  goes through the normal death respawn at their own last grace, and the world reloads [ARV,
  vanilla behaviour once nobody is held].
- 1.4.3: "You no longer accumulate rot essence when wiping out to a boss" [V S2].
  1.2.5: "Added a new event when a party dies to a boss" [V S2].

**Q4. Resting**
- "When one player rests at a Site of Grace, the world state will reset for all players" [V S1].
- "If one player sits at a bonfire while others are in boss rooms, those players will be removed
  from the boss rooms." [V S1] The locale string is "The power of grace expelled you from the
  arena" [V S4].
- There is no "all must rest" rule. One player resting resets everyone [V S1]. Exceptions:
  - "During an active invasion, you cannot warp or use sites of grace to rest" [V S1].
  - Evergaols need "All Wanderers must enter together" [V S4].
- 1.7.4: "Removed black screens when loading in enemies after resting at a bonfire" [V S2].
  The others' enemies come back in place without a loading screen. This matches the
  WCM+0x1E524 respawn flag in RE-ERSC §4.1 [ARV].
- Also on rest: Rot cured; spirit ashes once per grace rest (1.4.0) [V S2]; mod items granted
  "when you rest at a site of grace" [V S1].
- Warp notification: 1.7.9 "Warping to a bonfire will now inform other players in the session
  which bonfire you have warped to" [V S2]. ERSC 1.x had a grace **vote** for party warps
  (1.3.0, 1.4.3, 1.4.4) [V S2].

**Q5. Boss persistence**
- There is one shared host world, so a boss cannot be in two states inside one session [ARV].
  Progress written to your own save is "based on Host progression", and "Event flags toggled in
  the session may persist" [V S5].
- Lone fighter dies while the partner is far away (not in the fight): no primary source covers
  this case. By the rule text, the dead player stays in limbo until the partner rests or dies. A
  boss bar on the dead player's screen is enough [ARV, ?].
- FAQ field-boss workaround: "Disengage, warp and rest at a grace" force-resets the boss so the
  party can "engage bosses together". This fixes wrong health bars and missing rewards [V S5].
- FAQ known bugs: "Bosses dying prematurely, mid fight" (rare) [V S5]. 1.8.8: world event flags
  "inappropriately linger between reloads causing bugs with certain boss fights (e.g. Fire
  Giant, Radagon)" [V S2].

**Q6. Minibosses / field bosses / multi-phase**
- Field bosses have boss bars, so the boss-bar trigger applies to them as well [ARV from 1.1.0].
- Evergaols:
  - all must enter together [V S4];
  - "don't die in evergaols" [V S5];
  - 1.4.2 "evergaol bosses to have no multiplayer scaling" (fixed) [V S2].
- Multi-phase / special arenas [V S2]:
  - Radahn: partial completion for spectators (1.8.3);
  - Ainsel River and Nokron boss teleporters (1.7.9);
  - 1.3.6 "bosses found behind fog walls to not scale correctly" (fixed).
- Loot from "persistent enemy defeat (Bosses and Scarabs …)" is given to everyone who joins the
  host [V S5].

**Q7. Regular enemies**
- After respawning, a dead player sees enemies as the living partner left them: killed enemies
  stay dead until a rest or a wipe [V S1 Rot rationale].
- Late join: the joiner loads into the host's world, so it sees the host's live enemy state
  [ARV]. It also gets the host's last grace as its respawn point (1.5.0) [V S2].
- Known gaps [V S5, S2]:
  - "Shadow enemies … only become regular enemies for the host";
  - "desync or invisible enemies" → "Rest at a grace";
  - 1.3.0 "some dynamic geometry to become desynced after dying as a guest" (fixed).

**Q8. Recovery tools and known limitations**
- Rest at a grace is the universal resync [V S5]. Other recovery steps:
  - "Everyone mist out and rejoin the session" (for warp bugs) [V S5];
  - Separation Mist ends the session [V, RE-ERSC];
  - the community ER Debug Tool / save manager to fix flags [V S6].
- FAQ / workarounds list [V S5, S6]:
  - quest progression "can be erratic", so do quests solo;
  - session flags may persist into your save, so back up saves;
  - Wanderers do not sync progress if the host is behind them;
  - items already received cannot be gained again.

### 1.2 Dark Souls III Seamless Co-op (LukeYui, Nexus 1895, v0.1.1)

(Nexus 1574 is the older "Enhanced Co-op" page. The LukeYui mod is 1895.)
- Q1: same sentence as ERSC: respawn "at the last bonfire they rested at. The session will not
  be terminated." [V S7] 0.09: "When joining a session, you will now inherit the host's last
  bonfire respawn location" [V S7].
- Q2: "Dying during a boss battle or invasion will put your character into limbo, spectating
  other players until either everyone in the party has been defeated, or a player rests at a
  bonfire." [V S7] 0.05: "spectator mode to malfunction in a party greater than two" (fixed)
  [V S7].
- Q3: 0.04: "When dying to a party wipeout (e.g. dying to a boss) - enemies will now reset as
  expected" [V S7]. Before 0.04 a wipe did **not** reset enemies, which shows the reset has to be
  driven explicitly. 0.06: "If the host has used the taunters orb and dies to an invader or
  boss, the orb effect will be reset" [V S7].
- Q4:
  - "When one player rests at a bonfire the world state will reset for all players" [V S7];
  - "If one player sits at a bonfire while others are in boss rooms, those players will be
    removed from the boss rooms." [V S7];
  - 0.06 fixed "resting at a bonfire to remove multiplayer scaling effects" [V S7];
  - Windows Central: the mod "resets ongoing boss fights if a defeated player sits at a bonfire
    to respawn and attempt running back in" [S S13].
- Q5/Q6: "All fog walls/barriers that usually restrict the multiplayer zone are gone" [V S7].
  Boss fog walls still exist. 0.03 and 0.1.1 fixed fog-wall animations around the Dancer that
  forced "other players into animations" [V S7]. Lesson: a fog traversal by one player must not
  replay on others.
- Q7: 0.03: "Fixed a bug which caused some dead enemies to persist between player data slot
  reloads" [V S7]. Kill state is held in memory and has to be cleared on a reload.
- Q8: death counter and death messages (0.1.0), death message spam fix (0.1.1) [V S7].

### 1.3 Dark Souls Remastered Seamless Co-op (LukeYui, Nexus 899, v0.0.4)

- Q1–Q4: identical rule text to DS3: own last bonfire, limbo until everyone is defeated or a rest,
  rest resets the world for all, and "Defeating enemy bosses and clearing areas no longer sends
  co-operators home." [V S8] Scaling for every enemy via `ds1sc_settings.ini` [V S8].
- 0.0.3: "Fixed a bug which allowed bonfire resting and/or warping even when an invader is
  present" [V S8].
- 0.0.4: "Parchment of Deliverance" revives NPCs who died in your world (a recovery tool for
  permanent world damage); the death count was always 0 (fixed) [V S8].
- The "boss rooms" removal sentence is not in the DSR description [V S8, absent]. It probably still
  applies, since rest resets the world [?].

### 1.4 Nightreign Seamless Co-op (LukeYui, Nexus nightreign/3)

- It adds duo (and up to 6), its own exe and save, and scaling via `nrsc_settings.ini` [S S14].
  The changelog snippet mentions a "boss health bar desync caused by players getting different
  scaling" [S S14].
- I found no change to death rules. Nightreign's native downed/revive is used [S S14, ?]. It is
  not relevant to bosses beyond one lesson: scaling must be identical on all clients, or the bars
  diverge.
- "Nightreign Revive (Seamless CO-OP)" (Nexus eldenring/11175) is a third-party add-on for ERSC.
  It brings Nightreign-style ally revive to base ER [S S14]. This is a precedent for an optional
  "partner revive" (§3, R2 option).

### 1.5 Bloodborne Seamless Co-op (BBSC, CLSAMG22, v0.0.2, GPL; ideas only)

The CHANGELOG is the useful part. Most items are marked "untested" or "STRONGLY INDICATED" by
the author.
- **Adopted rules** (2026-09-16): "outside boss fights a dead player respawns at their last
  checkpoint in the same world, the world is not reset, the partner keeps playing; the world
  resets for everyone only when someone rests. During a boss fight the dead player spectates
  until the boss dies or everyone is dead." [V S15]
- **Held-death decision table** [V S15]:
  - "boss bar up + partner alive → spectate; when the bar has been gone for 2 s → respawn in place";
  - "no boss bar + partner alive → respawn in place right away";
  - "nobody alive (1.5 s) or session over → run the held step, normal reload".
- **Boss bar is cleared before the death step** (2026-09-30 b): the game's
  `Failed_BossAreaMission` "wipes all three boss gauge slots … before the death step, so
  BossFightActive() was always false". Fix: "a death counts as a boss death if a bar is up or
  was up within 10 s with no boss kill since". Spectate ends on the boss kill event, not on "bar
  gone" [V S15].
- **A load ends the fight** (2026-10-01 d): the guest's spectate "kept waiting for a boss kill
  after the load reset the boss". Fix: if the local character changed (a load) → respawn,
  "a load ended the boss fight" [V S15].
- **A dead copy looks alive** (2026-10-04 b): "a dead player's copy on the other machine keeps
  HP > 0", so the second to die "spectated forever". Fix: a life relay packet {id, dead, seq}
  "twice a second"; a partner counts as dead when "every fresh (3 s) report says dead"
  [V S15].
- **Wipe ordering** (2026-10-04 e): the guest saw nobody alive and reloaded itself into its own
  respawn point, and the host's copy of it then looked alive. Fix: "a summoned guest … never
  reloads itself while the session lives … until the host's reload rebuilds its character";
  "fallback own reload after 45 s". A later entry: on seeing the host alive again, the guest
  "waits 2 s and requests its own full reload into the host's map" [V S15].
- **Respawn point** (2026-09-24 g): "A dead guest respawned inside a boss arena (guest's own
  last lamp / nearest lamp of the map)". The new order is "host's last lamp if in the loaded map
  → beside the host → nearest lamp → own body" [V S15].
- **Two world copies drift** (2026-09-16): after a host respawn, "enemies healed from the guest's
  damage". When the guest no longer reloads on a host death, "enemies the host's reload resets
  may stay dead or out of place on the guest's screen" [V S15]. This is exactly our risk.
- **WORLD-SYNC** [V S16]:
  - enemy existence/death synced: CONFIRMED;
  - enemy respawn "follows host world rules, tied to lamp rest": STRONGLY INDICATED;
  - boss fog entry/spawn/fight synced: CONFIRMED;
  - boss HP and phase: UNKNOWN;
  - world/flags: host authority, CONFIRMED.
- **Shared progress**: boss-defeated flags go to the guest save through an allowlist; shortcuts,
  NPC and quest flags are not copied (user decisions) [V S16]. The wiki calls it "additive only"
  [V S18].
- **Known gaps** in the 2026-09-16 entry: "the world does not reset when a lamp is used by the
  partner"; the spectate camera did not follow the partner (later solved with an automatic free
  cam 2 s after spectate starts, mod 17) [V S15].
- **KNOWN-LIMITATIONS** [V S17]:
  - session bound to one loaded stage;
  - "Death persistence unfinished";
  - undocumented gameplay packet format;
  - no host migration.
- **Recovery tools** [V S15]:
  - F8: warp to your own position +0.5 m with a collision reset;
  - Silencing Blank: a clean leave;
  - "partner gone" safety net: no remote character for 90 s → suspend persistence patches;
  - F1: free camera.
- **Wiki** [V S18]: "A full party wipe can return the host to the Hunter's Dream instead of the
  last lamp" (a known issue); "Guests should see the host's current world state, including
  affected doors, bosses and lifts".

### 1.6 visy/bloodborne_pc_windows_port "party co-op" (a second, independent Bloodborne seamless port)

This port is not in the brief. It is the most thoroughly tested ERSC-parity effort found, with
loopback-live evidence and one real two-PC playtest, and its parity table cites the ERSC Nexus
text.

**Binding design decisions** (user, 2026-10-11) [V S19]:
- Death outside a boss: each player "respawns at their OWN last lamp … the others play on".
- Death during a boss fight: "Limbo: spectate until the boss dies, the party wipes, or someone
  rests". "Boss fights use a SHARED lamp: after a boss-fight death or wipe everyone respawns at
  the same lamp."
- Rest: "Resets enemies for everyone (ERSC); players in boss rooms are pulled out".
- Joining: "Joining during a boss fight / cutscene: Joiner waits outside … until it ends".
- Death debuff: implemented, but OFF by default since 0.7.2.

**Implementation details** [V S19]:
- C2 (host dies alone) is a deliberate divergence: one respawn load for the whole party at the
  host's lamp, because seamless glitches were worse than a loading screen. Since 2026-10-21:
  "when the guests killed the boss the host gets up in the arena instead, no load".
- C5 (limbo): "Limbo while a boss bar is up (EMEVD 2003[11] HUD slots)". The spectator's camera
  follows a living member, Cross switches target, a followed member that dies is replaced, and
  peers hide the dead copy (no body, no plate).
- C7 (rest during a boss fight): "during a boss fight it is always a real load (the fight and
  the enemies reset, everyone is taken out of the boss room)". A guest's own lamp use during the
  host's fight also pulls the party out.

**PR #6, boss defeat race** [V S20]. The host died against Gascoigne, the guests finished him,
and the respawn load ran ~1 s later. The defeat flag is written "only after the death animation",
so "the reload wins that race and resets the world". The boss came back in phase 2 at full HP.
The fix:
- wait while the boss is "active or dying" (bar up with a 0-HP character, or the bar just gone);
- add +3 s for the flags;
- cap the wait at 20 s.

After the kill, the host is revived in place. A guest revived in place "could not be hit until
its next load", so "Every guest death outside a boss fight now takes the respawn load".

**Issue #4** [V S21]:
- Stuck wipe: fixed by "a dead player now stays counted until they're revived, come back or
  leave".
- Fall deaths now always take a normal load.
- "enemies killed by the host in a neighboring area stayed alive for the guest" was fixed.
- Host death resetting the world "works as designed for now", with ERSC-style host death on the
  roadmap.

---

## 2. Cross-mod comparison (short)

| Case | ERSC / DS3SC / DSRSC | BBSC | visy bbport | Notes for Sekiro |
|---|---|---|---|---|
| Death outside boss | own last checkpoint, no world reset [V] | in-place respawn at last lamp, no reload [V] | own last lamp; guests take a load, host loads the party [V] | single death must not reset the shared world |
| Death in boss | limbo until wipe / rest (community: or kill) [V/S] | spectate until kill; gauge remembered 10 s [V] | limbo until kill / wipe / rest; shared lamp after [V] | gauge-cleared-before-death trap exists in BB; check Sekiro |
| Rejoin running fight | no (limbo) [V] | no [V] | no; joiners wait outside [V] | Sekiro has few fog gates, so enforce by rule |
| Wipe | everyone respawns, world resets (DS3 0.04), no Rot [V] | guest waits for host reload, 45 s fallback [V] | one load together [V] | host reload first, client follows |
| Rest | resets all; boss rooms emptied; no "all rest" vote [V] | partner lamp reset = known gap [V] | resets all; boss = real load [V] | already P2; add expel + spectate release |
| Boss state split | impossible (one world) [ARV] | host authority; HP/phase UNKNOWN [V] | host authority [V] | we have two worlds: host is the sole truth |
| Death penalty | optional Rot, cure at grace [V] | echoes drop [V] | rot optional, off by default [V] | Sekiro has a native penalty, keep it per player |
| Late join | host's last grace as respawn [V] | joins beside host [V] | waits outside boss/cutscene [V] | send dead-list + flags + host idol |
| Resync | rest at grace; mist out and rejoin [V] | F8 collision warp, Silencing Blank [V] | auto rejoin, party travel [V] | add "rest = resync", hotkeys |

---

## 3. Recommended rule set for the Sekiro mod

The baseline is ERSC's rule text, because players know it and the three LukeYui mods agree.
Where we diverge, the reason is one of four Sekiro or architecture facts:

- **A. Two synced worlds, not one native world.** Each player runs their own world with our
  host-authoritative enemy sync (IMPLEMENTATION-NOTES). Any local reload rebuilds that
  player's world from EMEVD and flags. Vanilla Sekiro's death reload restores every regular enemy
  and puts a boss back at full HP (RE-DEATH-RESET §1). In ERSC a held death is a nicety. For us,
  holding or replacing the reload is what keeps the worlds equal.
- **B. Resurrection.** Sekiro has a native in-place revive (Rise / Die, the `p+0x212C` /
  `p+0x2138` windows). HP 0 is not a death. Only the committed death (`base+0x3EA0D74` 0 → 1,
  RE-DEATH-RESET) counts for session rules.
- **C. Native death penalty.** Losing half the Sen and XP progress, Dragonrot and Unseen Aid
  already punish deaths, per player. ERSC needed Rot because vanilla ER's penalty is lost runes,
  which you can pick back up.
- **D. Deathblow phases and few fog gates.** Bosses and minibosses have 2–3 deathblow "lives"
  (phase = HP bar refills). A deathblow is a two-actor grab. Many fights (minibosses, several
  bosses) have no fog wall to stop a respawned player from simply walking back in.

### R1. Death outside a boss fight
1. Vanilla Resurrect stays fully local and is not a session event. While the Rise/Die choice is
   open, send `EV_DOWN(2)` ("downed") so the partner's overlay can show it.
2. On the committed death: the normal Sekiro death runs for that player only. Their own
   penalty and their own Unseen Aid roll apply, and Dragonrot spreads only in their save. Keep
   flags 9500–9599 unsynced (RE-DEATH-RESET P1). They reload at their **own last idol**
   (ERSC/DS3 [V]).
3. Exception: a player who joined and has not rested yet uses the **host's last idol** (ERSC
   1.5.0, DS3 0.09 [V]).
4. The world does **not** reset for anyone. After the dead player's reload, the mod re-applies
   the shared state (P3 `EV_BACK` / `BULK_DEAD_LIST`): every enemy dead in the authoritative
   world stays dead.
5. If the host is the one who died, the client is the keeper of the kill list during the host's
   reload. This is the case BBSC calls drift: "enemies healed from the guest's damage" [V S15].
   It must be explicit.
6. Use a fall death like any other death. visy found in-place revive after falls unreliable and
   forces a load [V S21].
7. Death debuff: offer ERSC-style `death_debuffs`, **off by default**, cured by resting at an
   idol. Reason C: the native penalty already exists. The setting exists only for the "partner
   keeps the world alive" exploit that ERSC names.

### R2. Death during a boss or miniboss fight
1. **"In fight" signal:** a boss gauge slot is up (`MenuMan+0x2E10`), **or** one was up within
   the last 10 s with no defeat since. This is BBSC's fix for the game clearing the gauge before
   the death step [V S15]. [LIVE] check whether Sekiro's own "player died in arena" EMEVD clears
   the gauge first.
2. **Spectate condition:** on the committed death, hold the reload (SPEC-BOSS-RULES §1, phase A
   then B) if a living partner is **engaged in the same fight**. Engaged means the partner's
   gauge shows the same boss entity, or the partner is inside the arena radius in the same
   block. Partner liveness comes from an explicit alive/dead relay (2 Hz, 3 s freshness), never
   from the remote copy's HP (ERSC 1.4.4, BBSC mod 10 [V]).
3. **Resurrect first:** the spectate starts only after "Die" or when no Resurrection is
   available. Reason B.
4. **While spectating:**
   - no rejoining the fight (ERSC/DS3/BBSC/visy all agree [V]);
   - the camera follows the partner;
   - the corpse is not targetable (ERSC 1.2.1, "enemies attack players who are spectating")
     and leaves no collision obstacle (ERSC 1.4.3).
   Reason D makes "no rejoin" mandatory: without a fog wall, a spectator released early would
   simply walk back in.
5. **The boss keeps its HP, posture and deathblow count** for everyone. The host's boss is the
   only truth. The spectator's world must not run its own reload while the fight lives (reason
   A), or its local boss would reappear at full HP and phase 1 (the visy Gascoigne bug [V S20]).
6. **Release** (one function, see SPEC §1.4):
   - (a) **Boss killed.** Release only after the defeat flag has been seen, waiting up to
     +3 s after the gauge drops and at most 20 s (visy PR #6 [V S20]). Then respawn at your own
     last idol. The boss stays dead through the synced defeat flag.
   - (b) **Wipe** → R3.
   - (c) **Any rest by anyone** → R4.
   - (d) **Partner left the block, disconnected, or went 5 s without a life report** → normal
     death reload.
   - (e) **Manual "give up" (F8).** Allowed only if the partner is not engaged anymore.
     Otherwise it would reset the spectator's local boss mid-fight. Show the reason.
   - (f) **Timeout of 15 min.**
7. **Spectator credit:** a spectator present at the kill gets the full defeat. Defeat flags are
   already shared, and the reward goes through `BULK_BOSS_REWARD`. This matches ERSC 1.8.3 and
   the FAQ's "persistent enemy" loot for everyone [V].
8. **Lone fighter dies while the partner is far away or not engaged:** ERSC would hold the dead
   player in limbo until the partner rests or dies [ARV]. For us that is a bad experience
   (minutes of spectating someone exploring) and an exploit (the boss stays damaged).
   Recommendation:
   - no spectate; take the normal death reload;
   - treat it as a **fight wipe for that boss**: the boss must return to full for both;
   - if the host died, the host's own reload resets the boss natively;
   - if only the client was in the fight, the host resets the boss by a silent rest-reset when
     the host is out of combat, or by a boss-only reset if RE finds one [LIVE/RE];
   - the gauge drop without a defeat (`EV_BOSS(3)`) tells the partner.
9. Optional, later: a "partner revive" (an ally resurrects you mid-fight at a cost) like the
   third-party "Nightreign Revive" for ERSC [S S14]. It is off by default, and it is the only way
   back into a running fight.

### R3. Wipe (every engaged player committed dead, or both players dead anywhere)
1. **Detection:** each player's committed-death state from the life relay. A dead player stays
   counted until revived, reloaded and alive again, or disconnected (visy issue #4 [V S21]).
   Near-simultaneous deaths count as a wipe.
2. **Order:** the host reloads first, at its own last idol. The client does not reload on its own
   while the session lives. It waits for the host's "back alive" event, then reloads into its own
   last idol. If the client's last idol is in another block, it uses the host's idol, so the pair
   lands together. Fallback: an own reload after 45 s (BBSC mod 12 [V S15]).
3. **Everything resets for both:** regular enemies and the boss arena. This is DS3 0.04, "enemies
   will now reset as expected" [V]. No extra death debuff for a boss wipe (ERSC 1.4.3 [V]); the
   Sekiro penalty was already paid at each committed death.
4. Never respawn anyone inside a boss arena. If the chosen idol is in the arena (it can exist only
   after the kill), use the next idol. This is the BBSC bug "A dead guest respawned inside a boss
   arena" [V S15].

### R4. Resting at an idol
1. One player resting resets regular enemies for **both** (ERSC/DS3/DSR [V]; already P2 in
   RE-DEATH-RESET). There is no "all must rest" vote.
2. If the partner is engaged in a boss fight when you rest, the partner is **expelled**: warped
   to their own last idol with a load, and told "Your partner rested: the idol's power pulled you
   from the arena". The boss resets. ERSC/DS3 text [V S1, S7]; visy C7 uses a real load
   [V S19]; SPEC §3.
3. A spectator is released and respawns (SPEC §1.4).
4. Rest cures the optional death debuff. Rest is also the documented **resync point**: on every
   rest, send the full flag + dead-list snapshot (ERSC FAQ: "Rest at a grace" for desync
   [V S5]).
5. Use `safe_to_warp()` (SPEC §3.3) before any expel warp. Never warp from under an open idol or
   talk menu (live crash 2026-10-23).
6. Idol travel always passes through a rest in Sekiro, so a travel resets first, then moves the
   traveller. Announce it like ERSC 1.7.9: "<name> travelled to <idol>".

### R5. Boss persistence
1. **One state per boss: the host's.** Defeat flags are shared both ways (existing).
2. **HP, posture and deathblow count:**
   - they are always the host's values;
   - a client's deathblow is a request to the host, and the host plays the grab;
   - no client-side phase change.
3. A phase change that plays a cutscene (e.g. a multi-stage final fight) must play for both or
   hold both. Treat it as a cutscene event [?, needs RE].
4. Scaling is locked at fight start (ERSC 1.2.5 [V]) and identical on both machines (Nightreign
   bar desync [S]).
5. **Starting alone is allowed** (user decision 2026-10-23); the partner gets a notice
   (`EV_BOSS(1)`). A partner who arrives alive may join the running fight, the same as ERSC's
   living players.
6. Only the dead are barred from re-entering (R2.4). A **joiner** who connects during a fight
   waits at the host's last idol until the gauge drops (visy decision [V S19]).
7. **Reflections / Remnant (Gauntlet) fights:** excluded from all session rules. SPEC already
   skips Remnant.

### R6. Minibosses (deathblow-marker enemies) and field fights
1. If a miniboss raises a boss gauge, R2 applies unchanged. [LIVE] verify on the Chained Ogre and
   General Naomori Kawarada whether minibosses call `SetBossGauge`. I am not certain they show the
   same gauge [?].
2. If they do not, use a miniboss list instead: `NpcParam` rows with more than one deathblow, or
   the entity ids from the defeat-flag table (`1AABBkxxx`, RE-FINDINGS-2). Apply R2 with a radius
   test: both players within 40 m of the miniboss and it has aggro.
3. Miniboss deaths are flag-only (RE-FINDINGS-2 [I]) and must be in the shared flag set. Sekiro
   Online's known gap was death sync "apart from bosses and minibosses" (WEB-OPEN-SOURCE-COOP-2
   §3).
4. Open-world minibosses have no arena to expel from. On a rest, R4.2 becomes "reset the
   miniboss and warp only if the partner is within the aggro radius". Otherwise the partner just
   sees the miniboss come back at full HP after the reset.

### R7. Regular enemies
1. A killed regular enemy stays dead for both while at least one player has not reloaded. This
   holds after a single death (R1), for the respawned player, and across a host reload (client
   keeps the list).
2. Everything comes back on a rest (R4) or a wipe (R3) only.
3. **Late join:** on connect, the joiner gets:
   - the host's flag snapshot (existing);
   - the host's current dead-list, i.e. the DSR `WorldInit` pattern (WEB-OPEN-SOURCE-COOP-2 §3);
   - the host's last idol as its respawn point.
   Enemies the host killed are dead for the joiner, as in ERSC's single world.
4. Clear the in-memory kill list on every rest, wipe and save-slot load. DS3 0.03: "dead enemies
   to persist between player data slot reloads" [V].
5. Neighbouring-block kills: a kill in a block the other player has not loaded must still reach
   them when they load it (visy issue #4 [V S21]). Keep the dead-list per block.

### R8. Player-facing recovery tools and limits (ship with the rules)
- **Rest at an idol = resync** (documented; ERSC FAQ precedent [V S5]).
- Hotkeys:
  - **F8 "wake at idol"** (spectate release, guarded by R2.6e);
  - **"unstuck"**: a warp to your own position with a collision reset (BBSC F8 [V S15]);
  - **"leave co-op"**: a clean session end (Separation Mist / Silencing Blank analogues [V]).
- Overlay per partner: alive / downed (Resurrect choice) / spectating / in boss <name, HP %> /
  death count (ERSC `overhead_player_display = 4` [V S3]).
- Safety nets:
  - no partner report for 90 s → drop the hold rules (BBSC [V S15]);
  - spectate timeout of 15 min;
  - defeat wait capped at 20 s.
- Known-limitations list (draft, mirrors ERSC FAQ): quests and NPC dialogue are best done solo
  or by the host; back up saves; a player behind in story may be pulled forward; Reflections and
  Gauntlets are excluded.

### Differences from ERSC / DS3SC at a glance

| Rule | ERSC/DS3SC | Sekiro mod | Why |
|---|---|---|---|
| What counts as death | HP 0 / death state | committed death only (`0x3EA0D74`) | B: Resurrect is a native in-place revive |
| Death penalty | Rot optional, default on (ER) | native Sen/XP/Dragonrot per player; extra debuff optional, default off | C |
| Spectate trigger | boss bar present | gauge up or up within 10 s **and** a living partner engaged in that fight | A + BBSC gauge-clear trap; avoids endless limbo when the partner is far away |
| Release on kill | when bar/fight ends | after the defeat flag (≤ 20 s wait) | visy PR #6 race; A: an early reload resets the local boss |
| Lone death, partner far | limbo until the partner rests or dies | normal death; the boss resets for both | A: there is no shared world to keep alive cheaply; D: no arena lock |
| Re-entry to the fight | blocked by limbo | blocked by limbo; joiners wait outside; optional partner revive | D: few fog gates |
| Host death | like anyone | like anyone, but the client keeps the kill list during the host reload | A |
| Minibosses | covered by boss bars | gauge if present, else list + radius | D: open-world, multi-life minibosses |

---

## 4. Open questions to settle live (cheap)

1. Does Sekiro's arena death EMEVD clear the boss gauge before `CSDeadStep`? (Log `boss: gauge
   off` timing vs the committed death.)
2. Do minibosses (Chained Ogre, Naomori, Juzou) set a boss gauge slot?
3. Defeat flag timing after the last deathblow: gauge-off → defeat flag delay (BB needed up to
   ~3 s).
4. Host-side boss-only reset: is there a function that resets one boss (HP, posture, EMEVD fight
   state) without a full reload? If not, R2.8 falls back to a silent rest-reset.
5. During a held death, does the boss AI keep targeting the living remote player? (SPEC §1.5,
   RE-ENEMY-TARGETING.)

---

## Sources

Primary [V]:
- S1 ERSC Nexus description: https://www.nexusmods.com/eldenring/mods/510 (read 2026-10-24 in a real browser)
- S2 ERSC Nexus changelog (1.0.1–1.9.7): https://www.nexusmods.com/eldenring/mods/510?tab=logs
- S3 ERSC 2.0.1 `ersc_settings.ini` (local extract `F:\Temp\re_ersc\extract\SeamlessCoop\ersc_settings.ini`)
- S4 ERSC 2.0.1 `locale/english.json` (same folder): `YKNX3_INFORMBEGINSPECTATEPLAYER`, `YKNX3_PLAYERDIED`, `YKNX3_EVERGAOLRADIUSERROR`, `ACTIONBUTTON_BOSSROOMKICKOUT`
- S5 ERSC FAQ: https://ersc-docs.github.io/faq/
- S6 ERSC Workarounds: https://ersc-docs.github.io/workarounds/ (and https://ersc-docs.github.io/how-to-install-and-update/)
- S7 DS3 Seamless Co-op Nexus description + changelog: https://www.nexusmods.com/darksouls3/mods/1895 , https://www.nexusmods.com/darksouls3/mods/1895?tab=logs
- S8 DSR Seamless Co-op Nexus description + changelog: https://www.nexusmods.com/darksoulsremastered/mods/899 , https://www.nexusmods.com/darksoulsremastered/mods/899?tab=logs
- S15 BBSC CHANGELOG: https://github.com/CLSAMG22/BBSC/blob/main/docs/CHANGELOG.md (entries 2026-09-16 … 2026-10-05)
- S16 BBSC WORLD-SYNC: https://github.com/CLSAMG22/BBSC/blob/main/docs/WORLD-SYNC.md
- S17 BBSC KNOWN-LIMITATIONS and SESSION-LIFECYCLE: https://github.com/CLSAMG22/BBSC/blob/main/docs/KNOWN-LIMITATIONS.md , https://github.com/CLSAMG22/BBSC/blob/main/docs/SESSION-LIFECYCLE.md
- S18 BBSC wiki: https://seamlesshunt.com/wiki
- S19 visy port ERSC parity table and design decisions: https://github.com/visy/bloodborne_pc_windows_port/blob/HEAD/docs/party/ERSC_PARITY.md (also `docs/party/seamless_rules.md`)
- S20 visy PR #6 (boss defeat race): https://github.com/visy/bloodborne_pc_windows_port/pull/6
- S21 visy issue #4 (wipe, host death, revive): https://github.com/visy/bloodborne_pc_windows_port/issues/4
- unseamless-coop (ER rewrite) SPECTATE.md / DEATH-DEBUFFS.md, used for mechanism context only: https://github.com/micthiesen/unseamless-coop/tree/HEAD/docs

Secondary [S]:
- S9 Steam: https://steamcommunity.com/app/1245620/discussions/0/4030224882302996449/ ("enter spectate until boss is dead or everyone else dies")
- S10 TheGamer / TechRadar on the original ERSC spectate design: https://www.thegamer.com/elden-ring-mod-seamless-multiplayer/ , https://www.techradar.com/news/elden-ring-co-op-mode-is-on-the-way-for-pc-players-and-im-not-jealous-at-all
- S11 Elden Ring Reforged options (fake boss bar for always-spectate; search snippet, page returned 402): https://err.fandom.com/wiki/Options
- S12 Steam, boss rewards when one dies: https://steamcommunity.com/app/1245620/discussions/0/4337599680248266964/
- S13 Windows Central on DS3SC: https://www.windowscentral.com/gaming/dark-souls-3-seamless-co-op-is-here-bringing-elden-rings-best-mod-to-one-of-fromsoftwares-best-games ; GameSpot: https://www.gamespot.com/articles/dark-souls-3-seamless-co-op-mod-lets-you-and-your-friends-fight-to-the-end/1100-6529705/
- S14 Nightreign Seamless Co-op (search snippets): https://www.nexusmods.com/eldenringnightreign/mods/3 , https://www.pcgamer.com/games/action/if-you-want-to-play-elden-ring-nightreign-in-a-duo-the-seamless-co-op-mod-is-already-out-dead-simple-to-use-and-it-wont-mess-up-your-vanilla-save/ , Nightreign Revive add-on: https://www.nexusmods.com/eldenring/mods/11175
- Gamepressure mirrors of the Nexus descriptions (ER/DS3/DSR): https://www.gamepressure.com/download/elden-ring-seamless-co-op-v174-mod/zf13deb , https://www.gamepressure.com/download/dark-souls-iii-seamless-co-op-v008-mod/zc15141 , https://www.gamepressure.com/download/dark-souls-remastered-seamless-co-op-v002-mod/zd151db

Not reached / not checked: the ERSC 2.x changelog entries beyond 2.0.0/2.0.1 (the Nexus log
page lists 1.x only), DS2 Seamless Co-op (LukeYui) death rules, and AC6.
