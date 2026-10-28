# Web research: enemy AI on the client, world state on join, ghost appearance (2026-10-22)

Scope: public sources only. No closed binaries opened or downloaded (no ERSC, no Sekiro Online, no DS3 Seamless). Everything is in my own words.
Tags: **[V]** = I read the primary file this session. **[X]** = two or more independent public sources agree. **[S]** = search summary only. **[blocked]** = site returned 403 (Nexus changelog).
Game version: "1.06" below means Sekiro 1.06 (SekiroTool calls it "1.6.0"; the file version string starts `1.6.0.`). All RVAs are module-relative (add the `sekiro.exe` base, image base `0x140000000`).
Not repeated here: items already in `NETCODE-RESEARCH.md`, `WEB-ANIM-RESEARCH.md`, `RE-FINDINGS-2.md` (ForceAnimation, FreezeAi bit, team type setter `0x9F5170` were already known; I only add cross-checks).

Local scratch copies of the files I read: `F:\Temp\claude\C--Users-<user>-Documents-Default-Project-projects\1b4983ae-1b36-41a7-9459-828a695fbbc0\scratchpad\w\` (Offsets.cs, offsets.rs, ela.CT, ss_*.cs/rs).

---

## 1. Enemy AI on non-host machines

### 1.1 What the big co-op mods and native netcode do (public facts only)

| Source | What it says | Tag |
|---|---|---|
| Souls Modding wiki, "DSR GameData Communication" (https://soulsmodding.com/doku.php?id=topic%3Adsr_gamedata_communication) | Native FromSoft packet list (Dark Souls Remastered, same engine family). Enemy **position** (`WorldChrSyncLocation`, #3) is sent as distance from the enemy's default spawn point, by every player, continually; which player is allowed to move a given enemy is "unknown variables". Enemy **health** (#4) is sent continually by clients. Enemy **AI state** has two one-way packets: guest->host (#20) and host->guest (#21, sent shortly after a guest connects). **EnemyAction** (#33) is an array of key/value pairs that sets enemy animations and behaviours (attacks, idle, death) by writing into a WorldChrMan sub-struct. **EnemyInit** (#39): one packet per enemy with initial location, actions, health, sent to a joining player. **WorldInit** (#15): host only, lists which enemies are dead plus blood decals. **RequestWorldChrSyncInitInfo** (#8): a new guest asks the host for current world state. | [V] |
| same page, team/phantom packets | **TeamChrType** (#9) resends a player's character type on update; **PlayerData** (#6) is sent once by a guest to the host and includes phantom type. | [V] |
| Meaning for us | Native design is not "host runs AI, clients get positions". It is a hybrid: every machine simulates, enemy state is nudged by action/position/health packets, and the host seeds the world on join. Clients are puppeted by **animation/action replication**, which is the model our `hkbsync` already approximates. The cleaner version of the same idea: client AI off, host positions + action keys on. | inference |
| ERSC docs FAQ (https://ersc-docs.github.io/faq/) | Desynced or invisible enemies: "rest at a grace" (resting resets the world for everyone). Lock-on problems: reload character. Field-boss health bars / missing rewards: disengage, warp, rest at a grace. Authors do not explain the sync design. | [V] |
| seamlesscoop.net/coop (https://seamlesscoop.net/coop/) | Boss victories and completed progression events "carry into each player's world". Resting at a grace resets world state for everyone ("necessary to prevent enemy desync" per the Nexus feature text, [S]). Peer-to-peer over Steam networking, own save format. | [V]/[S] |
| ERSC changelog (Steam announcement, https://steamcommunity.com/app/1245620/discussions/0/4406291166503639205/) | 1.7.2 (6/30): full rewrite, "enemies falling through the floor" gone, event flags properly synchronised. 1.7.3: lock-on code (CSLockTgtMan) rewritten; invasions disabled during boss fights; shop stock local per save. 1.7.7: interactable objects (elevators, fire pillars) resetting - fixed. 1.7.8 (pulled) / 1.7.9: a bug flipped some event flags when sent to other players (stonesword-key gates), "serious world event disruption". 1.8.8 (9/28): world event flags lingering between reloads broke Fire Giant and Radagon; event flags syncing wrongly when invading a player with more progress; removed test code that caused enemy-damage problems. | [V] |
| Nexus ERSC changelog (https://www.nexusmods.com/eldenring/mods/510?tab=logs) | 403 for fetch. Search summary: 2.0.1 phantom-colour bug fix; 2.0.0 game 1.17 compat; 1.9.6 desync between host preferences and others. | [S][blocked] |
| Sekiro Online (Nexus 577, https://www.nexusmods.com/sekiro/mods/577; coverage: Kotaku/GamesRadar/TechRaptor) | Author's own description: pure P2P, "fully functional world, event, and enemy sync", "a degree of client prediction" for PvE, joining a host in another world state (Burning Ashina) loads the area and enemies correctly, players level-scaled. No internals published. | [S] |

Lessons the authors' own changelogs give (all about **event flags**, not enemy AI): the pain was never "enemy AI differs", it was flags leaking (inherited from the other player, lingering across reloads, flipped on transit). Plan the flag filter early.

I found **no** public statement of which side runs enemy AI in ERSC or DS3 Seamless. I looked; the authors do not say. Do not assume "AI only on host" is documented anywhere.

### 1.2 Disabling AI in Sekiro (concrete, three independent sources agree on #1)

**A. Global debug flag byte "AllNoUpdateAi" (all NPCs, whole game):**

- Sekiro **1.06**: `sekiro.exe + 0x3D7A376`, one byte, 1 = AI of every character stops updating. Absolute `0x143D7A376`.
- Three sources agree:
  1. SekiroTool `Offsets.cs`: `DebugFlags.Base = module + 0x3D7A366` (1.06), `DisableAi = 0x10` -> `0x3D7A376` [V]. `EnemyService.ToggleDisableAi` writes a bool there.
  2. libsekiro table copied in `mstampfli/sekiro-coop` `offsets.rs`: `debug_flags = 0x3d7a369` (1.06) and `all_no_update_ai = 13` -> `0x3D7A376` [V].
  3. ElaDiDu's CT: entry "All No Update Ai", `DebugFlags+10`, live address `143D7A376` [V]. SoulSplitter `chr_dbg_flag.rs` calls it `AllNoUpdateAi = 16` from its own base [V].
- Other versions (SekiroTool `DebugFlags.Base`): 1.02 `0x3B67F8C`, 1.03/1.04 `0x3B68FCC`, 1.05 `0x3D7A2C6`, 1.06 `0x3D7A366`. DisableAi offset is `0x10` on 1.05/1.06 and `-0x26` on 1.03/1.04 (relative to that base). Careful: libsekiro bases are +3 (`0x3d7a369`) and use different indexes; do not mix base and index from different tables.
- Neighbours in the same byte array, 1.06, SekiroTool naming: AllNoDeath `+0xB`, AllNoDamage `+0xC`, AllNoHit `+0xD`, AllNoAttack `+0xE`, **AllNoMove `+0xF` (= `0x3D7A375`)**, DisableAi `+0x10` (= `0x3D7A376`), AllNoPosture `+0x17`. Note: our `RE-FINDINGS-2.md` line 641 talks about a "config byte `0x3D7A375`" that zeroes MoveSpeedLevel for non-local, non-"remote" characters. That byte is the **AllNoMove** debug flag. Consistent with the name; worth double-checking you are not toggling it by accident.
- Caveat: global. It also stops the host's own AI if set on the host. On the **client only** it would turn off every enemy's thinking, and the client would then rely entirely on host-driven animation/position. Untested by us. Your note says per-ChrIns FreezeAi + hook dropped FPS from 60 to 15 and did not move the character, so test the global byte separately before trusting it. I could not find any public report of the HKB/animation behaviour of an enemy under AllNoUpdateAi.

**B. Per-character FreezeAi bit (one enemy):**

- `ChrIns + 0x1F40` is a byte block; **FreezeAi = byte +1, mask `1<<4`** i.e. `*(u8*)(chr + 0x1F41) |= 0x10`. NoHit `+0` bit5 (0x20), NoAttack `+0` bit6 (0x40), NoMove `+0` bit7 (0x80), NoGoodsConsume `+2` bit4, NoEmblemsConsume `+3` bit0 [V, SekiroTool `Offsets.cs`; `BitFlagsStart` has no version switch, so it holds for 1.02-1.06]. Already in your `RE-NOTES.md`; I only confirm the source constants.
- SekiroTool uses it from `TargetService.ToggleAiFreeze` on the locked target's ChrIns [V].

**C. AI-driver pokes (not "off", but steer):**

- ChrIns `+0x58` = ComManipulator; AI think object = `*(chr+0x58)`'s `+0x340` ("AiThink" in SekiroTool; the CT walks `[[chr+0x58]+0x340]` for these) [V from both].
- AiThink fields: `+0x7B30` TargetingSystem ptr (with TargetView bit at TargetingSystem `+0x5A` bit 3), `+0xB741` ForceAct (byte), `+0xB742` LastAct, `+0xB743` ForceKengekiAct, `+0xB744` LastKengekiAct [V].
- `WorldAiMan` global (1.06 `0x3D55070`): `+0x4C6EC` GlobalForceAct, `+0x4C6ED` GlobalForceKengekiAct [V]. Other WorldAiMan RVAs: 1.02 `0x3B422D0`, 1.03/04 `0x3B43310`, 1.05 `0x3D54FD0`.
- Idea: a client can steer the enemy with ForceAct (the act index the host's AI chose) instead of freezing it. `LastAct` from the host could be replicated at 1 byte per enemy. Act numbers are per-enemy AI script ids (SekiroTool ships `GameIds/AiActs.cs` for the dragon). Unverified that it is deterministic enough to replace position sync.

**D. Other AI-adjacent items:**
- Params: `NpcParam` (Sekiro param index 6 in the CT) has `thinkId`; `NpcThinkParam` and `NpcAiActionParam` exist [V CT]. Changing NpcThink for one chr would need a re-create.
- `AllNoHit/AllNoAttack/AllNoMove` give a "dummy enemy" state without touching AI ids.
- ER/DS3: I did not find a public, citable AllNoUpdateAi/NoThink offset for them via search (DS3 TGA and igromanru tables came up but I could not extract a flag). Not verified.
- The Souls Modding wiki has no page listing a per-ChrIns NoAI flag for Sekiro (search returned none).

### 1.3 Existing Sekiro co-op code you can read

`github.com/mstampfli/sekiro-coop` (Rust, 1.06; the GitHub license endpoint returned nothing, so treat as all-rights-reserved until confirmed and copy facts only) [V]:
- Authority model: host owns enemies, broadcasts enemy state at 5 Hz filtered by a 64-bit digest per entity; client applies HP decrement-only and ignores inbound enemy claims from the non-host side. Entity registry fed from hook arguments, **not** from WorldChrMan scans (their scans crashed on stale data, same lesson as your crash).
- Its blocker list: no visible remote player; ApplyDamage AOB unknown; **no AI-disable method documented** (`SetCharacterAiState` only as an EMEVD opcode). Position writes to ChrIns "accepted but ignored by the render path".
- Their planned workaround: patch `common.emevd.dcx` so the game's event VM calls the natives (RequestAnimationPlayback, ForceAnimationPlayback, CharacterWarpRequest, SetCharacterAiState, SetCharacterTeamType, ChangeCharacterEnableState). Not started.

---

## 2. World state when joining a host

### 2.1 What ERSC does (public statements)

- "Progress is saved based on Host progression" and flags sync **to the host**; wanderers (joiners) behind the host do not sync well, quest progression erratic, shop and crafting availability follow the host's world, bell-bearing turn-ins host only (https://ersc-docs.github.io/faq/ [V]).
- Event flags toggled in a session "may persist" into the joiner's own save, so progression can end up mismatched; recommendation: keep a manual backup before joining someone with different progress (same FAQ [V]; https://ersc-docs.github.io/workarounds/ [V]).
- Workaround docs: for wrong Erdtree state, set flags 300/301/302 by hand with a debug tool while solo. Shows the flags are ordinary game event flags that leak into the joiner's save [V].
- Own save format (`.co2`-style, kept apart from vanilla) [V seamlesscoop.net].
- Items: each player gets their own drops; unique items (Golden Seed, Sacred Tear) must be picked individually; "wanderers can't re-gain items already received in their own world" [V FAQ].
- Rest at grace -> world state reset for everyone (the mod's way of fixing enemy desync) [V seamlesscoop.net].
- Conclusion (inference from the above, not an author statement): ERSC **copies/syncs event flags in both directions at runtime and persists the result into each player's co-op save**; it does not simply load "the host's save". The cost is flag leakage, which the changelog above shows was their dominant bug class.
- Vanilla DSR native co-op: `WorldInit` packet (host only) tells the guest which enemies are dead; `RequestWorldChrSyncInitInfo` pulls the current world state [V wiki]. Native approach is "host snapshot at join, small, one-shot".

### 2.2 Sekiro event flag storage (precise, 1.06)

Source: SoulSplitter `src/SoulMemory/Sekiro/Sekiro.cs` (GPLv3, `FrankvdStam/SoulSplitter`), which re-implements Ghidra'd `GetEventFlag` [V].

Globals / functions (1.06 RVAs, all agree between SekiroTool and ElaDiDu's CT [V]):

| Name | RVA 1.06 | Notes |
|---|---|---|
| `EventFlagMan` global (SprjEventFlagMan) | `0x3D55FE8` | pointer-to-struct; SekiroTool `EventFlagMan.Base`. Others: 1.02 `0x3B43248`, 1.03/04 `0x3B44288`, 1.05 `0x3D55F48` |
| `GetEvent(man, flagId) -> bool(al)` | `0x6C3E60` | CT symbol `GetFlag` = `1406C3E60`; SekiroTool `Functions.GetEvent`. 1.02 `0x6C15A0`, 1.03/04 `0x6C1600`, 1.05=1.06 |
| `SetEvent(man, flagId, value, 0)` | `0x6C4520` | CT symbol `SetFlag` = `1406C4520`. Calling shape from SekiroTool asm: `rcx = *EventFlagMan, rdx = flagId, r8 = value(0/1), r9 = 0`. 1.02 `0x6C1B90`, 1.03/04 `0x6C1BF0`, 1.05=1.06 |
| `GetChrInsByEntityId(entityId)` | `0x6C1A10` | 1.02 `0x6BF190`, 1.03/04 `0x6BF1F0` |
| `FieldArea` global | `0x3D5C0A0` | `+0x18` world-block owner, `+0x20` GameRend, `+0x28` CurrentWorldBlockIndex |

Version-independent AOBs (from SoulSplitter, usable instead of RVAs) [V]:

- `SprjEventFlagMan` ptr: `48 8b 0d ? ? ? ? 48 89 5c 24 50 48 89 6c 24 58 48 89 74 24 60` (rip-rel at +3, instruction length 7, then deref).
- `set_event_flag` function start: `40 55 41 54 41 55 41 56 48 83 ec 58 80 b9 28 02 00 00 00 45 0f b6 e1 45 0f b6 e8 44 8b f2 48 8b e9` (the first arg `rcx` has a byte at `+0x228` checked as "flags enabled"; SoulSplitter hooks it and reads `rdx` = flag id, `r8` = value).
- `get_event_flag` function start: `40 53 48 83 ec 20 80 b9 28 02 00 00 00 8b da`.
- ElaDiDu's CT uses: GetFlag `8b da 74 ?? e8 ?? ?? ?? ?? 4c 8b c0 48 85 c0 74 ?? b8 d3 4d 62 10` (offset -13); SetFlag `45 0f b6 e1 45 0f b6 e8 44 8b f2 48 8b e9` (offset -19).
- `WorldChrManImp`: `48 8B 35 ? ? ? ? 44 0F 28 18` (rip-rel at +3, len 7); position chain `[WorldChrMan] -> +0x48 -> +0x28`, XYZ at +0x80/+0x84/+0x88. Player ChrIns = `[WorldChrMan] + 0x88` [V].
- Debug-flags block: `80 3d ? ? ? ? 00 0f ? ? ? ? ? 48 8b 9b d0 11 00 00` (rip-rel at +2, len 7; this resolves to `DebugFlags` base, i.e. SekiroTool base `0x3D7A366` - the SoulSplitter enum starts at PlayerNoDead=0 and AllNoUpdateAi=16, which matches `0x10`).
- `MenuMan`: `48 8b 05 ? ? ? ? 0f b6 d1 48 8b 88 08 33 00 00`.
- emevd dispatcher (to log or replay instructions): `40 53 56 57 48 81 ec 40 01 00 00 48 c7 44 24 20 fe ff ff ff 0f 29 b4 24 30 01 00 00`.

Flag id decomposition (for id `N`), exactly as in SoulSplitter:
```
div10M  = (N / 10000000) % 10       // selects top-level array: *(man + 0x218) + div10M*0x18
area    = (N / 100000)   % 100
div10K  = (N / 10000)    % 10
div1K   = (N / 1000)     % 10
cat     = (area >= 90 || area + div10K == 0) ? 0 : 1 + (category found by walking FieldArea worldinfo: for each 0x38-byte entry whose byte@+0xB == area, look at its 0xB0-stride block list at +0x28 (count byte@+0x20), match block id byte (flag>>0x10 & 0xff) == div10K and (flag>>0x18) == area, then take int@+0x20 of that block)
base    = *(*(man + 0x218) + div10M*0x18) + cat*0xA8 + (div1K << 4)   // pointer; then dereference once more -> bit array
word    = bitarray[ ((N % 1000) >> 5) * 4 ]  (u32)
mask    = 1 << (0x1F - ((N % 1000) & 0x1F))
```
So a flag is one bit in a u32 inside a per-area, per-block bitset; reading/writing the bit directly works (SoulSplitter does both), **but the category lookup depends on which world blocks are currently loaded**, so for flags in a map that is not loaded `GetEvent` can fail (returns false) or the write has no home. Using the game's own `GetEvent/SetEvent` functions (table above) is safer than hand-writing bits.
Flags with area >= 90 (global flags such as 9300-9317, 6xxx, 8xxx) live in category 0 and need no world lookup.

### 2.3 Sekiro flag ids you can use right now

From ElaDiDu `Sekiro-Practice-CT` (`Ela_Sekiro_Table.CT`, "Bosses" list, line ~10600) [V]. The CT only labels them as boss/miniboss flags; I did not test that they are the "defeated" bit. The global 93xx ones match SoulSplitter's `Boss` enum, so those are defeat flags at least [X].

- **Chained Ogre (Ashina Outskirts): `11100310`**. **Chained Ogre (Ashina Castle): `11110620`**. (A third Chained Ogre in Feeding Grounds is mentioned by a checklist site, flag unknown to me.)
- Other Outskirts: General Naomori Kawarada `11100300`, General Tenzen Yamauchi `11100301`, Headless (Ako) `11100330`, Shigekichi `11100480`, Gyoubu Oniwa `9301`, Demon of Hatred `9313`.
- Hirata: Lady Butterfly `9302`, Owl (Father) `9317`, Juzou `11000300`/`11000301`, Enshin `11000330`, Masanaga (Hirata) `11000353`.
- Ashina Castle: Genichiro `9303`, Great Shinobi Owl `9308`, Isshin Ashina `9316`, Blazing Bull `11110440`, Matsumoto `20005340`, Jinsuke `11110504`, Masanaga (Castle) `11110316`, Vilehand `11110305`, Ungo `11110660`, Ujinari `11110511`.
- Reservoir: Genichiro (reservoir) `9300`, Isshin Sword Saint `9312`, Shigenori `11120390`, Shikibu `11120530`, Shume `11120521`, Longswordsman `11120450`.
- Abandoned Dungeon: Shichimen Warrior `11300200`. Mibu: Mist Noble `11500200`, Gachiin `11500680`, Tokujiro `11500490`, O'Rin `11500690`, Corrupted Monk (spirit) `9306`.
- Sunken Valley: Guardian Ape `9304`, Headless Ape `9314` (note: SoulSplitter's enum says `9307`; the two tables differ on Headless Ape - verify live), Snake Eyes `11700200`/`11700201`, Gokan `11700500`, Centipede Giraffe `11700425` (speedrun.com Shura splits use `11705202`, different flag), Shichimen `11700520`.
- Senpou: Folding Screen Monkeys `9305`, Armored Warrior `12000400`, Sen-Un `12000279`. Fountainhead: Corrupted Monk (real) `9309`, Divine Dragon `9310`, Okami Leader Shizu `12500406`, Shichimen `12500580`, Yashariku `12500550`, Sakura Bull `12500570`.
- Other useful ones in the same CT: Demon Bell `9830`, Charmless (no Kuro's charm) `6911`, Ashina Castle invasion `8301`, Ashina Castle fire `8302`, Hirata fire `8304`, tutorial `8306`, Headless Ape `8303`, Emma skip `11115900`, Emma fight `71110020`, escaped snake `11100850`, dragon phase-2 trees `12505888`, final dragon attack trigger `12505889`, Isshin moved to castle `71100500` [V SekiroTool `GameEvent.cs`].
- Numbering convention (visible in the data): 9xxx/6xxx/8xxx = global; 1AABBnnnn style = `1` + map area (`11` Outskirts, `10` Hirata, ...) etc.; id `11100310` -> area `11`, div10K `0`, block 0 of Ashina Outskirts. Flag ids are `m11_00_00_00` event scripts' flags; the Souls Modding wiki "Sekiro EMEDF" (https://soulsmods.github.io/emedf/sekiro-emedf.html) documents `HandleBossDefeat(entityId)` as the instruction that sets a boss's defeat flag in maps m10_00, m11_00, m11_01, m15_00, m17_00, m20_00 [S].

### 2.4 Hooking to replicate flags (already proven by others)

- Hook `SetEvent` (`rdx` = id, `r8` = value): done in SoulSplitter (JmpBack hook) and `sekiro-coop` (SetFlag detour validated live) [V]. Replay by calling the original with `(EventFlagMan, id, value, 0)`; `sekiro-coop` bypasses its own hook while replaying to avoid echo.
- `sekiro-coop` found that some flags are re-asserted every frame (their example `11_105_730`), so edge-trigger (send on change only) or you flood the link [V].
- Wanted for a join snapshot: iterate the flag ids you care about (boss lists above plus the 6xxx/8xxx/9xxx globals and idols `12500015`-style) and `GetEvent` each one on the host; send as a list; apply on the joiner with `SetEvent`. That is the ERSC model reduced to a whitelist.
- Idols: the CT's "Unlock All Idols" lists flag ids such as `12500017` (Feeding Grounds), `12500018` (Near Pot Noble), `12500015` (Sanctuary) [V].

---

## 3. Making the ghost look like a normal player (not a phantom)

### 3.1 What drives the phantom look

- `PhantomParam` is a param table in **Sekiro too** (Paramdex `SDT/Defs/PhantomParam.xml`, PHANTOM_PARAM_ST) with edge/front/diffuse-multiply/specular-multiply/light colours [V]. In the Paramdex names file the row labels (white/blue/red/gold/black phantom...) look copied from the DS3 template; Sekiro has no player phantoms, so I do not trust them for Sekiro rows. Check the rows in the real `Param` repository before relying on any id.
- `NpcParam` (Sekiro param index 6 in the CT, `SoloParamRepository` base +0x78 + 0x48*6) fields [V CT + Paramdex]: `drawType` (u8, +309 / `0x135`), `npcType` (+310), **`teamType` (+311 / `0x137`, "who can hit/target whom")**, `moveType` (+312), `phantomShaderId` (s32, **+516 / `0x204`**, "id of the phantom param row to apply", default -1), `multiPlayCorrectionParamId` (+520). `-1` = no phantom shader = normal rendering.
- Runtime: ChrIns `+0x70` = "Character Type" (4 bytes, dropdown CHAR_TYPE) and `+0x74` = "Team Type" (byte, TEAM_TYPE) in the CT [V]. The dropdown contents were not in the CT I read. Your RE already found setter `0x9F5170(chr, type)` and recompute `0x9F64D0` (RE-FINDINGS-2 line 485).
- Team enum values from `sekiro-coop` `enums.rs`: -1 default, 0 disabled, 1 human, 2 white phantom, 3 black phantom, 4 hollow, 5 wandering phantom, 6 enemy, 7 strong enemy, 8 ally, 9 hostile ally, 10 decoy enemy, 11 child of red, 12 friendly enemy, 13 invader, 19 host, 20 coop, 21 hostile, 23/24 enemy1/2, 26 friendly npc, 27 hostile npc, 28 coop npc [V]. These are a shared FromSoft numbering; the repo does not say they were verified on Sekiro except "Host=19, Coop=20, Hostile=21, Enemy=6". Treat 19/20/21 as the Sekiro-confirmed set.
- How other games turn the colour off: community recipe for Elden Ring (Nexus 4512 "Removed or Decreased Phantom Colors", https://www.nexusmods.com/eldenring/mods/4512): edit `PhantomParam`; to look normal set edge/front/light alpha to 0, diffuse-multiply alpha 1 and diffuse colour 255 [S]. A DS3 commenter reported setting the NPC invader rows' phantom shader to 0 in `NpcParam` [S]. Both say the other side needs the same param, because params are per game install.
- In FromSoft MP the phantom look is applied by the **phantom shader id on the spawned character** (picked from the character/team type of the multiplayer slot), not by the model; the model for a joined player is the same player model as a normal player. That is what the DS3/ER wiki and mod posts imply; I have no Sekiro-specific confirmation.

### 3.2 How to apply this to our ghost

- Cheapest, if the ghost is built from an NpcParam row: use or clone a row with `phantomShaderId = -1` and `drawType` of a normal NPC. Done in the param repo at runtime (CT shows how to reach rows) or by shipping a regulation edit.
- If the ghost is a cloned PlayerIns / ReplayGhostIns, find where the shader id is read (ChrIns character-type change calls `0x9F5170`); setting character type to a non-phantom value should drop the shader. Worth trying: ChrIns `+0x70` = Human-like value and `+0x74` = Coop (20) so friendly fire rules stay right.
- I found **no public write-up** of how Sekiro Online or any DS3/ER mod hides the phantom look in the runtime (closed mods; not examined).

---

## 4. Ranked leads (max 8)

1. **Global AllNoUpdateAi byte `sekiro.exe+0x3D7A376`** (three sources agree) - stop all client-side enemy AI in one byte write. Test: toggle on the client only, then check that a host-driven position + `hkbsync` event still animates the enemy. Also check FPS (your per-ChrIns FreezeAi cost 60 -> 15). Cheapest experiment for the "two AIs fight" defect.
2. **Event flag join snapshot via the game's own `GetEvent`/`SetEvent`** (`0x6C3E60` / `0x6C4520`, `(EventFlagMan, id, value, 0)`, EventFlagMan `0x3D55FE8`) with a boss/idols whitelist (`11100310` Chained Ogre Outskirts, `11110620` Castle, global `93xx`, the lists in 2.3). Matches the ERSC model minus its leaks.
3. **Edge-triggered `SetEvent` hook, two-way, with echo suppression and a whitelist/blacklist** (SoulSplitter and sekiro-coop already prove the hook; ERSC changelog shows leaks/lingering/flips are the real bug class; their 1.8.8 fix is "flags linger between reloads" -> clear the replicated set on session end).
4. **Phantom look: `NpcParam.phantomShaderId` (+516, `-1` = off) and ChrIns char type `+0x70` / team type `+0x74`** (setter `0x9F5170`). Try character type switch to non-phantom and team 20 (coop) on the ghost.
5. **AI steering instead of freezing: `[[chr+0x58]+0x340]` AiThink: `+0xB741` ForceAct / `+0xB743` ForceKengekiAct; LastAct `+0xB742`.** Replicate the host's last act byte per enemy, apply on the client with AI otherwise running. Good fallback if AllNoUpdateAi blocks animation. Only 1 byte per enemy per change.
6. **Adopt the native packet model as the design reference**: WorldInit (dead-enemy list, host only, once), EnemyInit (location + action + HP at join), EnemyAction (anim/behaviour key-values), Location as distance-from-spawn (smaller, spawn-relative). Source: Souls Modding wiki DSR page. Spawn-relative positions also cut bytes per enemy.
7. **Use `sekiro-coop`'s lesson: enemy registry from hook args, not from WorldChrMan sweeps** (their sweeps crashed on stale pointers; your crash dump history points the same way).
8. **Flag-id table to test live: confirm that `11100310` and `11110620` flip to 1 on Chained Ogre death** (and that the Headless Ape id is `9314` vs `9307`) with a SetEvent-hook logger; SoulSplitter has a built-in event flag logger you can use as a reference or for ground truth.

## 5. Gaps (could not find)

- Which side runs enemy AI in ERSC/DS3 Seamless: not documented publicly.
- Public per-enemy NoAI/DisableAI offsets for DS3/ER: not found via search (the DS3 TGA table was not parsed).
- Sekiro Online rendering internals (closed, intentionally not examined).
- CHAR_TYPE/TEAM_TYPE enum lists for Sekiro from the cheat table (links present, tables absent in the file I read).
- ElaDiDu/SekiroTool licence text: SekiroTool is MIT (read), SoulSplitter GPLv3 (copy no code, facts/offsets are fine), CT repo and sekiro-coop licence not confirmed.

## Sources

- SekiroTool: https://github.com/borgCode/SekiroTool (files `SekiroTool/Memory/Offsets.cs`, `Services/TargetService.cs`, `Services/EnemyService.cs`, `Services/EventService.cs`, `Resources.resx`)
- Sekiro-Practice-CT: https://github.com/ElaDiDu/Sekiro-Practice-CT (`Ela_Sekiro_Table.CT`)
- SoulSplitter: https://github.com/FrankvdStam/SoulSplitter (`src/SoulMemory/Sekiro/Sekiro.cs`, `Boss.cs`, `src/soulmemory-rs/src/games/sekiro/game.rs`, `chr_dbg_flag.rs`)
- sekiro-coop: https://github.com/mstampfli/sekiro-coop (`README.md`, `docs/HANDOFF.md`, `crates/sekiro-sdk-sys/src/offsets.rs`, `crates/sekiro-sdk-core/src/enums.rs`)
- Souls Modding wiki DSR netcode: https://soulsmodding.com/doku.php?id=topic%3Adsr_gamedata_communication
- Paramdex Sekiro defs: https://github.com/soulsmods/Paramdex/tree/master/SDT (`Defs/NpcParam.xml`, `Defs/PhantomParam.xml`)
- Sekiro EMEDF: https://soulsmods.github.io/emedf/sekiro-emedf.html
- ERSC: https://seamlesscoop.net/coop/ ; https://ersc-docs.github.io/faq/ ; https://ersc-docs.github.io/workarounds/ ; https://steamcommunity.com/app/1245620/discussions/0/4406291166503639205/ ; https://www.nexusmods.com/eldenring/mods/510?tab=logs (403)
- Phantom colour edits: https://www.nexusmods.com/eldenring/mods/4512 [S]
- Sekiro Online (public description only): https://www.nexusmods.com/sekiro/mods/577
- speedrun.com Shura splits (flag examples): https://www.speedrun.com/sekiro/resources/hotox [S]
