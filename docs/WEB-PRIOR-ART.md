# Prior art: public Sekiro multiplayer work and what it teaches us (2026-10-22)

Scope: public sources only. No closed binary was opened, downloaded or run (no Sekiro Online, no ERSC, no Yui releases). Source files of open repos were read as text. We copy FACTS (offsets, approaches, lessons), never code, in our own words.

Tags: **[V]** = I read the primary file/page this session. **[X]** = two or more independent sources agree. **[I]** = my inference from verified facts (needs a live test). **[S]** = search summary only. **[blocked]** = HTTP 403.

Not repeated here (already in `WEB-WORLD-AI-RESEARCH.md`, `NETCODE-RESEARCH.md`, `WEB-ANIM-RESEARCH.md`): AllNoUpdateAi `0x3D7A376`, FreezeAi bit `chr+0x1F41 & 0x10`, EventFlagMan/GetEvent/SetEvent RVAs and flag decomposition, ForceAnimation RVAs, team enum, boss flag list, native DSR packet list. I only cross-check or extend those.

Local scratch copies of everything I cloned or fetched: `F:\Temp\claude\C--Users-<user>-Documents-Default-Project-projects\1b4983ae-1b36-41a7-9459-828a695fbbc0\scratchpad\repos\` (SekiroTool, fromsoftware-rs, sekiro-coop, unseamless-coop, me3) and `...\scratchpad\w\`, `...\web\`.
---

## 0. Landscape in one table

| Project | URL | Licence | Status | Value to us |
|---|---|---|---|---|
| sekiro-coop (mstampfli) | https://github.com/mstampfli/sekiro-coop | AGPL-3.0-or-later per README (no LICENSE file in tree) -> facts only | Rust + me3, 1.06, 6 commits, last 2026-06-15, NOT playable | Only open Sekiro co-op code. Documents the failure modes (position writes ignored, no AI-off, no damage hook). |
| Sekiro Online (LukeYui / Yui) | https://www.nexusmods.com/sekiro/mods/577 ([blocked] direct fetch; text via search/press) | closed binaries (untouched). Org repos below are public text. | Last release 2.05, 2021-06-13, unmaintained; works but buggy | Proof that the engine's vestigial netcode can carry phantoms + enemy + flag sync. Bug tracker is a lesson list. |
| sekiro-online/sekiro-online (bug tracker) | https://github.com/sekiro-online/sekiro-online | no licence, text only | 22 issues, 2021 | Real-world failure list (section 2). |
| sekiro-online/params | https://github.com/sekiro-online/params | no licence (game data) | CSV dump of every Sekiro param, edited | Shows the retained MP param tables (PhantomParam, NetworkParam, MultiPlayCorrectionParam, TentativePlayerParam...). |
| Sekiro Seamless Co-op (Yui) | https://www.patreon.com/yuiamoroll (403); status via search | closed | Not released as of 2026-10-22; Yui says skeleton code written, semi-functional, on ice [S] | Do not wait for it. |
| SanguineLoki700/sekiro-seamless-coop | https://github.com/SanguineLoki700/sekiro-seamless-coop | MIT | created 2026-10-20, C++, "0% verified", launcher + saves + protocol scaffolding only | Almost nothing for us (see 1.3). Never claims a spawn. |
| SekiroTool (borgCode) | https://github.com/borgCode/SekiroTool | MIT (copyright Shilkey, Centz) | Active, 1.02-1.06 | Best offset source: hook sites, functions, ChrIns/module offsets. |
| Sekiro-Practice-CT (ElaDiDu) | https://github.com/ElaDiDu/Sekiro-Practice-CT | no licence -> facts only | CE table, 1.06 | Param layouts, debug manipulators, flags. |
| SoulSplitter | https://github.com/FrankvdStam/SoulSplitter | GPL-3.0 | Active | Flag read/write AOBs (already used). |
| libsekiro / sekiro-practice-tool (veeenu) | https://github.com/veeenu/sekiro-practice-tool | AGPL-3.0 | Active | Base address table per version (also copied into sekiro-coop). |
| fromsoftware-rs (vswarte) | https://github.com/vswarte/fromsoftware-rs | MIT-NON-AI / Apache-2.0-NON-AI (the licence forbids use as ML training data; I used it only to read facts) | Active; sekiro crate is thin, DS3/ER crates rich | Engine structure homology (WorldChrMan, ChrSet, manipulators, net sync buffers). |
| DarkScript3 Sekiro EMEDF | https://github.com/AinTunez/DarkScript3 | no licence on file | Active | The retained network/multiplayer EMEVD instruction set. |
| unseamless-coop (micthiesen) | https://github.com/micthiesen/unseamless-coop | MIT | Elden Ring Rust rewrite of ERSC, NOT playable; spawn of remote ChrIns is its unsolved gate | Cautionary parallel (section 6). |
| des-seamless-coop (himingal) | https://github.com/himingal/des-seamless-coop | GPL-3.0 | Demon's Souls on RPCS3, reimplements the online server | Not applicable (needs the game's own session). |
| yuiamoroll/Sekiro-Debug-Patch | https://github.com/yuiamoroll/Sekiro-Debug-Patch | no licence | 2021 | Only enables the debug menu. Confirms Yui hooks with MinHook from a dinput8 proxy. |
| Souls Modding wiki, DSR packets | https://soulsmodding.com/doku.php?id=topic%3Adsr_gamedata_communication | wiki | read | Native packet model (section 5). |

I found no other public Sekiro multiplayer/netplay repo on GitHub (`gh search repos` for sekiro + coop/multiplayer/seamless/netcode/online/phantom/invasion/puppet/dll hook; hits were only the ones above) and no GitLab, Nexus (403) or Reddit page with technical content.

---

## 1. Per-project facts

### 1.1 mstampfli/sekiro-coop (AGPL, facts only)

Read: `README.md`, `docs/HANDOFF.md`, `docs/GAPS.md`, `crates/sekiro-sdk-sys/src/{offsets,aob,natives,live,chrins}.rs`, `sekiro-sdk-core/src/enums.rs`, `sekiro-sdk-bridge/src/{ai,combat}.rs`, `sekiro-coop-dll/src/hooks.rs` [V].

Architecture
- Raw UDP peer-to-peer, MinHook, me3 loader (Arxan disabled, online blocked). Six detours: SetFlag, ApplyEffect (SpEffect apply), DeleteEffect, GiveItem, AddExperience, WarpNextStage_Bonfire. AOBs for 13 natives plus 17 global symbols (list in 4.4).
- Host owns enemies. Host broadcasts enemy state at 5 Hz, filtered by a 64-bit per-entity digest (97% skipped when idle). Client applies HP decrement-only through `ChrIns+0x1FF8 -> +0x18 -> +0x130` and ignores inbound enemy claims from the non-host.
- Enemy registry fed from hook arguments (the `entity` argument of the ApplyEffect detour gave 42 live ChrIns), NOT from WorldChrMan scanning. Their scans crashed on stale pointers; the harness gates all cross-ChrIns reads on "IGT advancing for N consecutive frames".
- Remote events are applied by calling the trampoline directly so replays never re-enter the detour (no echo).
- Flags are edge-triggered. The README notes some flags (example `11_105_730`) are re-asserted every frame, so level-triggered sending floods the link.
- Wire: bincode enums; any variant added mid-enum breaks compat with older peers.
- Desync detector: state hash every 60 frames, three strikes kills the session.

What they could NOT do (their own words)
- Show the remote player. "Writing positions into ChrIns memory is accepted but ignored by the render path." They wrote `chr+0x1FF8 -> +0x68 -> +0x80` of a borrowed NPC and read back the same value, but nothing moved on screen. [V]
- Find native function addresses for RequestAnimationPlayback, ForceAnimationPlayback, CharacterWarpRequest, SetCharacterAiState, SetCharacterTeamType, ChangeCharacterEnableState (they only had the EMEVD opcode numbers). Their planned workaround was to patch `common.emevd.dcx` and make the game's own event VM call the natives from a flag trigger. Never started.
- Disable AI. No method documented. Damage-application hook also unknown ("no damage-application hook; client-side hits on bosses rely on host broadcast").
- Their design note: "Sekiro wasn't built for multiplayer, there is no phantom player slot like DS3/ER, all puppets must be NPC hijacks." This is the opposite of what the engine actually has (section 3), so treat it as a belief, not a finding.

Lessons for us
1. Raw writes to the physics module position (`+0x80`) are overwritten each frame. SekiroTool's NoClip proves the engine accepts a position written INSIDE the physics update (hook 4.3 `UpdateCoords`), not before/after it. If your own ghost-move code writes outside that window, expect "accepted but ignored".
2. Registry from hook args is crash-proof; sweeps are not. We already agree.
3. Edge-trigger flags and suppress echo. We already do (netcode doc); keep.
4. They validated a full replay of `EnemyStates` with a peer-simulator on one machine. Cheap regression idea for us.

### 1.2 Sekiro Online (closed; public description and tracker only)

Facts from the Nexus page text (via search/press), Kotaku/PC Gamer/Siliconera, the sekiro-online org [V for tracker, S for Nexus/press]
- Pure P2P over Steam, up to 6 players (1 host + 5 phantoms), matchmaking via a central server run by the author (issue #1, 2021-07). Session is area-restricted: new session after area change. Phantoms and invaders scale to the local player's level.
- Changelog 2.04 (largest): networking for destructible objects, enemy state flags and player item-use visuals; better sync of player and enemy effects; fixed desync when more than 2 incoming connections; fixed "phantom event triggers", "player instances" and engine out-of-memory bugs; reworked mod memory management to reduce summon-sign crashes. 2.05: Steam client compat.
- The mod uses the game's own multiplayer machinery (summon signs, Cursed Emblem, phantoms, invasions). It is therefore evidence that the retained DS3-lineage netcode in `sekiro.exe` carries remote players with normal player models and syncs enemies. How it spawns them is not public; I did not look at the binary.
- A third-party inspection note (SanguineLoki700 repo, 1.3) lists strings seen in Sekiro Online 2.05's dinput8.dll: `WorldChrMan`, `NetPlayerMemoryManager`, "remote-player type metadata", Steam callbacks. [V, secondhand]. Name to search for in our RTTI dump (`rtti-dump-2026-10-22.log`): `NetPlayerMemoryManager`.
- Status of the author's successor: Yui's Oct 2026 Patreon update lists Sekiro as the last of the series, "TBA", skeleton code written, semi-functional, ice [S]. Do not plan around it.

Reported problems (user level) [S, Steam thread 2022]: lag/stutters, random disconnects, "enemies and the host invisible", no difficulty scaling. One user's anecdotes, not confirmed.

### 1.3 sekiro-online bug tracker (22 open issues, by the mod's testers/contributors incl. the me3 author)

All [V] (read each issue body). Lessons are the useful part:

| # | Issue | Lesson for us |
|---|---|---|
| 2, 3 | Packets that must not be lost (EMEVD flag updates, hello) were sent unreliable; only ONE in-flight packet per type was kept, so two flag updates in a frame desynced the game | Reliable + queue (never "latest wins") for flags/events. Matches our hkb ack plan. |
| 10 | Signs spawn you at beginning of area | Position of a joiner must come from the host's live position, not area default. |
| 11 | Host occasionally invisible to phantoms/dark spirits | Visibility of a remote character can fail one-sidedly. Add a "ghost visible?" self-check + respawn. |
| 12 | Lady Butterfly's door (events) not synced | Door/cutscene events are separate from boss-defeat flags. |
| 13 | Other players' textures do not load (default skin) | Remote equipment/texture streaming is a separate failure from the model. Expect a grey/default skin on a spawned player-model unless equipment is loaded. |
| 14 | "Living Force" burn status missing: atkOccurenceSpEffectId not networked | On-hit SpEffects are not synced for free; per-attack side effects must be replayed (SpEffect id 106020/106021 example). |
| 15 | Prosthetic tools vanish from the world due to event sync: picking up an item in the host world that "becomes" a prosthetic permanently converts the joiner's copy into a Jizo Statue | Do not sync item-pickup event flags blindly. Whitelist. |
| 16 | Dodging into a player freezes posture regen: roll hitboxes (AtkParam_Pc 210 and 211) register as a hit in PvP | Sekiro dodge/dash carry real attack hitboxes (object-break). Setting DamageLevel 0 on those two rows fixed it without hurting PvE. Relevant if friendly fire is ever on. |
| 18 | Multiple lock-on points per Sekiro (one at feet) | Cosmetic; Sekiro model has several target points. |
| 19, 20 | Deflect posture damage skipped for bullets; poison/burn not scaled | Bullets (projectiles) are a separate path from melee. |
| 21 | Players become invisible in certain spots: crossing invisible culling ranges (Sunken Valley start, Outskirts snake, branches before Bodhisattva idol) | The remote model is culled by map streaming/ranges. Expect the ghost to vanish in the same places; plan a force-render/disable-cull check. |
| 22 | Hitstun and posture break animations occasionally fail to play, worse at high latency | Hit-reaction animation is driven by an event that can be lost; resync state not just events. |

Params repo (`sekiro-online/params`, CSV dump of the whole regulation) [V]: confirms the retained multiplayer tables exist in Sekiro's regulation: `PhantomParam` (29 rows, same 5-colour layout as DS3), `NetworkParam`, `NetworkAreaParam` (area-limited session timers 30/20/30 s, enabled per map group, e.g. 300000, 310000, ..., 991000), `NetworkMsgParam`, `MultiPlayCorrectionParam`, `MultiHPEstusFlaskBonusParam`, `MultiSoulBonusRateParam`, `WhiteSignCoolTimeParam` (180/360 s), `TentativePlayerParam`. NpcParam columns (1.06): `networkWarpDisp`, `ghostModelId`, `isGhost`, `isChangeWanderGhost`, `isMultModeBoss`, `TentativePlayerId`, `MultiPlayCorrectionId`, `drawType`, `npcType`, `teamType`, `moveType`. NpcParam has no `phantomShaderId` column in this dump (the CT's "+516" is something else, check before relying on the earlier note).

### 1.3b SanguineLoki700/sekiro-seamless-coop (MIT)

Created 2026-10-20, 7 commits, C++/CMake. README says "not a playable mod; 0% verified". Content: launcher with PE/SHA-256 identity, protocol v2 envelopes, save-path policy (`.co2` extension idea), read-only probe that reads `0x143D7A1E0` and prints the RTTI name `.?AVWorldChrManImp@NS_SPRJ@@`. It inspected Sekiro Online 2.05 and the DS1/DS2/DS3 Seamless packages statically with a decompiler (closed binaries; we do not do that). Observed 1.06 file facts: Steam build 5794815, `sekiro.exe` 68,005,144 bytes, SHA-256 `637aca52...b856`, file version 1.6.0.0; `.text` is encrypted on disk (~8 bits/byte entropy), so offline disassembly of the exe is useless without a runtime image. Also: Sekiro writes saves to `%APPDATA%\Sekiro\<steamid>\`, `.sl2`, with `.bak` copies. Nothing else usable.

### 1.4 SekiroTool (MIT) [V]

The best offset source. 1.06 values in section 4. Beyond what the earlier docs used:
- Hook sites with the asm it injects there (decoding gives register meaning): HpWrite, DamageMultiplier(+Deflect), UpdateCoords (NoClip), SetWarpCoordinates/Angle, InfinitePoise, FreezeTargetPosture, AddSubGoal, SetLastAct, AiHasSpEffect.
- Calling functions from an external thread: it builds small asm stubs; `ApplySpEffect(rcx = ChrIns*, edx = id)`, `RemoveSpEffect(rcx = [ChrIns+0x11D0], edx = id)`, `GetChrInsByEntityId(rcx = entity id) -> rax`, `Warp(rdx = idol id, r8d = 0)`, `ForceAnimation(ecx = entity id, edx = anim id)`.
- Waiting for a load: `MenuMan` byte `+0x20` goes 1 -> 0 when a loading screen starts and 0 -> 1 when it ends (the tool waits for exactly that transition).
- ChrIns player-only debug bits at `chr+0x1F40..` and the ChrData bitflags at `data_module+0x228` (NoDeath bit 2, NoDamage bit 3, NoPostureConsume bit 4).

### 1.5 ElaDiDu Sekiro-Practice-CT (no licence, facts only) [V]

- "Control Enemy Dbg" (CT entry 1337272092): the game ships a debug feature to drive an NPC with the player's input. The script does four writes: `[[npc+0x50]+0x358] := [WorldChrManDbg+0x150]`; `[[WorldChrManDbg+0x150]+0x300] := [[npc+0x50]+0x10]`; `[[WorldChrMan+0x88]+0x1F40] := 0xC0` (player NoAttack + NoMove so the player body stays idle); `[WorldChrManDbg+0x160] := npc`. Meaning: an NPC's controller object (`chr+0x50`) has a manipulator slot at `+0x358`, and the engine can swap which manipulator drives it (section 3.1). [V script, I interpretation]
- Param layouts for NpcParam, PhantomParam, MultiPlayCorrectionParam, NetworkParam/NetworkAreaParam (same as the Sekiro Online params).
- NPC fields via `targeted npc`: ThinkId at `[chr+0x30]+0x624`, NpcId `[chr+0x30]+0x628`, name `[[chr+0x30]+8]`, Handle `chr+0x08`, CharacterId `chr+0x68`, char type `chr+0x70` (4 bytes, dropdown CHAR_TYPE), team type `chr+0x74` (byte, dropdown TEAM_TYPE).
- AI think object: `[[chr+0x58]+0x340]`, ForceAct `+0xB741`, LastAct `+0xB742`, ForceKengekiAct `+0xB743`, LastKengekiAct `+0xB744`, Lua timers at `+0x7C`... (CT lists "Lua Timers" at +0x7c under the AI object), TargetingSystem `+0x7B30`.

### 1.6 libsekiro / sekiro-practice-tool (AGPL) [V via sekiro-coop copy]

Base address table per version (module-relative). The version names in libsekiro: V1_02, V1_03_04, V1_05, V1_06. 1.06 row: quitout `0x3d67408`, render_world `0x3b01838`, debug_render `0x3d77fa4`, igt `0x3d5aac0`, player_position (WorldChrMan) `0x3d7a1e0`, debug_flags `0x3d7a369`, show_cursor `0x3d8990c`, no_logo `0xe1b51b`, font_patch `0x263bc14`, debug_show `0x3d7a388` (this is WorldChrManDbg), grapple_debug `0x3d6d640`, fps `0x3e9f748`. Index of AllNoUpdateAi is 13 from `0x3d7a369` (= `0x3D7A376`). Note quitout `0x3d67408` is the MenuMan pointer (quitout flag is `MenuMan+0x23C`), consistent with fromsoftware-rs `sprj_menu_man_ptr = 0x3d67408`.

### 1.7 fromsoftware-rs (MIT-NON-AI; facts only) [V]

The `sekiro` crate only has: event flags (`sprj/event_flag.rs`), FieldArea, GameDataMan, MenuMan, param repository, world info. RVAs in `rva_data.rs` (1.06): `field_area_ptr 0x3d5c0a0`, `game_data_man_ptr 0x3d5aac0`, `map_item_man_ptr 0x3d6cdc0`, `map_item_man_grant_item 0x91c970`, `lua_event_man_remove_item 0x67b230`, `register_task 0x11df640`, `sprj_menu_man_ptr 0x3d67408`, `solo_param_repository_vmt 0x2b9ab48`, `world_info_vmt 0x29dcaa0`, `world_res_vmt 0x29dd190`, `app_menu_new_menu_system_ptr 0x3d8a140`, `global_hinstance 0x3e9edb8`. (`register_task` is the game's own per-frame task registration: a candidate hook-free way to run code on the game thread.)

Event flag storage (the clean statement of what SoulSplitter's maths implies): `SprjEventFlagMan` starts with `FD4VirtualMemoryFlag`: two data buffers (+0x00/+0x10 pointers, +0x10/+0x18 lengths), a `blocks` pointer at +0x28, two `EventWorld`s of 0xF8 bytes each (so worlds at +0x30 and +0x128), `current_world` pointer at `+0x218`, `current_world_index` at `+0x220`, init flag at `+0x228` (the byte `GetEvent` tests). `EventWorld` = 10 `EventRegion`s of 0x18 bytes (region = flag / 10,000,000 % 10). A region holds a `blocks` pointer + count; an `EventBlock` is 0xA8 bytes = 10 zones of 0x10 bytes + 8; a zone is a pointer to 32 u32 words. Index: block = world-block index + 1 found by (area = flag/100000 % 100, group = flag/10000 % 10) in the loaded FieldArea world infos; zone = flag/1000 % 10; word = flag % 1000 / 32; bit = 31 - (flag % 1000 % 32). Flags with area >= 90 (global 6xxx, 8xxx, 9xxx) are not addressable this way in the typed crate (they live in the area-0 category in SoulSplitter's maths). Two worlds exist and only one is current: the game double-buffers flag storage. [V] This matches section 2.2 of `WEB-WORLD-AI-RESEARCH.md`.

Engine facts from the DS3 and ER crates that plausibly apply to Sekiro (shared lineage; see 3):
- DS3 `WorldChrMan` begins `vtable, world_info_owner(0x8), world_area_chr_len(0x10), ptr(0x18), world_block_chr_count(0x20), ptr(0x28), pad(0x30), player_chr_set(0x38), ghost_chr_set(0x50), debug_chr_set(0x68), main_player(0x80)`. Sekiro matches with a +8 shift after 0x30: SekiroTool has main player at `+0x88`; libsekiro's position chain `[WorldChrMan]+0x48 -> +0x28 -> +0x80` is exactly `player_chr_set.entries` (at +0x48 = DS3 0x40 + 8) -> `ChrSetEntry` (stride 0x38, first field the ChrIns pointer, `+0x28` = physics module pointer) -> `+0x80` position. Two independent Sekiro facts fit the shifted DS3 struct, so I rate the shift [I, high]. Predicted Sekiro layout: player set capacity `+0x40` / entries `+0x48`, ghost (bloodstain/replay) set `+0x58`/`+0x60`, debug set `+0x70`/`+0x78`, main player `+0x88`, second-player slot (DS3: "maybe the owner of the host world") `+0x90`, loaded world-block list count `+0xC8`, pointers `+0xD0` (32 entries). DS3 `WorldBlockChr` has its enemy `ChrSet` at `+0x80`. This is the enemy list we need to avoid id sweeps; not verified on Sekiro.
- DS3 module container order (matches Sekiro where known): data `0x18`, resist `0x20`, behavior `0x28`, behavior_sync `0x30`, ai `0x38`, super_armor `0x40`, event `0x58`, physics `0x68`, action_request `0x80`, hit_stop `0x90`, **damage `0x98`**, material `0xA0`, knock_back `0xA8`, behavior_data `0xB8`. A `damage` module at `container+0x98` is a hook candidate for hit handling [I].
- DS3 `PlayerIns` has `player_session_holder`, `net_ai_manipulator`, `replay_recorder` fields after `player_game_data`.
- DS3 `ChrIns` field map up to 0x80 is the same shape as Sekiro's: `+0x08` handle, `+0x50` controller (player_ctrl), `+0x58` manipulator, `+0x68` char id, `+0x70` char type (i32), `+0x74` team type.
- ER-only (do not copy offsets): `ChrType` enum Local 0, WhitePhantom 1, Duelist 2, Ghost 3... Npc 5 ... GrayPhantom 8 ...; `CSDebugChrCreator` (set `init_data.name = "cXXXX"`, npc/think/charaInit params, entity id, position, then `spawn = true`) is the documented ER way to spawn any character.

### 1.8 DarkScript3 Sekiro EMEDF (facts) [V, parsed the JSON]

Sekiro's EMEVD still contains the multiplayer instruction set. Class[index] name :: args:
- 2000[2] Set Network Sync State :: Disabled/Enabled
- 2003[25] Place NPC Summon Sign / 2003[51] Summon NPC :: sign type, summoned NPC entity id, spawn point entity id, summon event flag, dismissal event flag
- 2003[62] Set Network-Connected Event Flag :: flag, state; 2003[63] Batch Set Network-Connected Event Flags :: start, end, state
- 2003[70] Set Network Interaction State; 2003[78] Send NPC Summon Home
- 2004[1] Set Character AI State :: entity, enabled; 2004[2] Set Character Team Type; 2004[3] Character Warp Request :: entity, warp type, destination entity, dummypoly; 2004[41] Issue Short Warp Request
- 2004[17/32] Request Character AI Command(+Event Value); 2004[19] Set Character AI ID; 2004[20] Request Character AI Re-plan
- **2004[28] Set Network Update Authority :: entity, authority level (0 Normal, 4095 Forced)**; **2004[34] Set Network Update Rate :: entity, use fixed frequency, frequency (-1 no update, 0 always, 2, 5)**
- 2004[58] Set Multiplayer-dependent Buffs (Non-Boss); 2004[67] Enable AI State Change From Enemy Team; 2009[4] Activate Multiplayer-dependent Buffs
- Conditions: IF/SKIP/GOTO/END IF Multiplayer State (0 Host, 1 Client, 2 Trying to create, 3 Trying to join, 4 Leaving, 5 Failed), Number of Co-op Clients, Number of Clients of Type (0 Co-op, 1 Invader, 2 Betrayal), Friendly Phantom Count, Multiplayer Event, Multiplayer Network Penalized; `WAIT For Network Approval`.
- Team Type enum includes 19 Host, 20 Co-op, 21 Hostile, 22 Wandering Phantom 1, 23/24 Enemy 1/2, 26 Friendly NPC, 27 Hostile NPC, 28 Co-op NPC, 29 Indiscriminate, 30 Object, 31 White Berserker, 32 Red Berserker.

Meaning: the retained FromSoft design has per-entity network authority and update rate, "network-connected" event flags (a flag range the engine syncs itself inside a session), and NPC summons. These are the engine's own answers to the problems we are solving by hand.

### 1.9 unseamless-coop (MIT, ER) [V docs, skimmed]

An open Rust rewrite of ERSC on fromsoftware-rs. Status in its own words: finds peers via a password lobby, authenticates over Steam, syncs settings, shows presence toasts and nameplates; **does not place players in each other's world**. Its last documented result (2026-07-25): both machines reach `Host`/`Ingame` with roster `players=2`, yet zero phantoms exist in `player_chr_set` on either side and the remote entry stays `join_wait=true`; the unsolved gate is "what clears join_wait and drives the ChrIns spawn". Takeaways: (1) even in ER, with an SDK, nobody open has produced the remote player spawn outside the native session; the spawn is a consequence of the session state machine, not a standalone call. (2) Its `SCALING.md` found the right lever for per-player enemy health: apply the SpEffects named by `MultiPlayCorrectionParam` rows (referenced from `NpcParam`) rather than mutating NpcParam HP. Sekiro has the same tables (`MultiPlayCorrectionParam`, NpcParam `MultiPlayCorrectionId`); in the Sekiro Online dump only rows 140101 and 140201 resolve to existing neutral SpEffects, so this lever is mostly unused in vanilla Sekiro. (3) ERSC facts: ER "Seamless" uses its own save format, resets the world for everyone when anyone rests at a grace, and an ERSC changelog line says chariots target only the host "to prevent conflicting AI goals" (already in `NETCODE-RESEARCH.md`).

### 1.10 Other seamless mods, public statements only

- DS3 Seamless (Yui): persistent session across deaths/boss kills/area changes, world state resets for all when anyone rests, NPC dialogue/events synced, progress carries into each player's own world, optional enemy health/damage scaling, custom Steam networking layer, up to 6 players, separate save. Fixed bugs worth copying as test cases: resting at a bonfire removed the multiplayer scaling effects; enemy scaling wrong; talk-event item awards unsynced; enemies not resetting after party wipe. [S]
- DS2 Seamless (alpha 2026-09-22): known issues are exactly ours: "dying in a session can cause enemy desync for already-alerted enemies" (two bosses named), object interactions (doors opened, items picked up) not updating in the phantom's own world, item pickups duplicated or lost on reload, host world occasionally un-joinable until reopened. [S]
- ER Seamless: see `WEB-WORLD-AI-RESEARCH.md`.
- No Yui mod documents which side runs enemy AI.

---

## 2. What the retained engine already contains (why this matters)

Sekiro 1.06 contains the DS3-era multiplayer subsystem, disabled but intact: MP params (1.3), EMEVD network instructions (1.8), `Multiplayer State` enum, `WhiteSignCoolTime`, summon-sign code paths (sekiro-coop's AOB list includes `Upgrade/OpenSkillsMenu` and a "enlarge unk hkb array" site), and the ChrIns fields (`+0x70` char type, `+0x74` team type, `+0x58` manipulator). Sekiro Online ran on top of it. The sekiro-coop author's belief that "there is no phantom slot" is contradicted by that, by the char-type/team-type fields, by `ghost_chr_set`, and by the NpcParam columns `isGhost`/`ghostModelId`/`isChangeWanderGhost`.

---

## 3. Answers to the three questions

### (a) Public, working way to spawn a normal-looking, animatable player-model character in Sekiro?

**No.** No public project has one working. Specifics:
- sekiro-coop: tried borrowing an existing NPC and moving it by raw writes; failed (1.1). Planned EMEVD patch route not done.
- SanguineLoki700: no spawn, probe only.
- unseamless-coop (ER): same wall (spawn gated by the session FSM), 1.9.
- Sekiro Online: works, but closed and driven by the game's native session. Nobody has published how.
- Leads that are real, in descending value:
  1. **Reuse the engine's remote-player path rather than fake a PlayerIns.** The native path creates a `PlayerIns` in `player_chr_set` for each remote player, with char type White Phantom (DS3 enum) and a Network manipulator (3.1). Our spawn work should be judged against "what makes the engine put a PlayerIns into `WorldChrMan+0x48` entries" (set capacity at `+0x40`).
  2. **EMEVD route**: `Summon NPC` / `Place NPC Summon Sign` (2003[51]/[25]) are the game's own NPC-spawn-with-flag triggers; sekiro-coop's patch plan (set a flag from the DLL, the VM runs the instruction) is the same trick our team could use without any native address. Requires shipping a patched `common.emevd.dcx` via an override (me3 does that).
  3. **ER pattern to test for in Sekiro**: a debug character creator object reachable from WorldChrMan (`CSDebugChrCreator` in ER; sets name/params/position then flips a `spawn` byte). Sekiro has `WorldChrManDbg` (`0x3D7A388`, +0x6F draw flag, +0x150 and +0x160 used by "Control Enemy Dbg"); whether it also holds a creator is unknown. Cheap probe: dump `WorldChrManDbg` and look for a pointer whose RTTI contains `Creator`.
  4. **Look**: the phantom look in DS3 comes from the character type: DS3 CHAR_TYPE dropdown (from The Grand Archives DS3 table, `chr+0x70`): 0 Host, 1 White Phantom, 2 Dark Spirit, 3 Ghost, 4 Ghost, 8 Hollow, 10 Transparent, 11 Invisible, 12 Dragon, 13 Arena; TEAM_TYPE (`chr+0x74`) 1 Host, 2 Phantom, 3 Black Phantom, 4 Hollow, 6 Enemy... Sekiro uses the same offsets and the same team ids in EMEDF (2 White Phantom, 3 Black, 19 Host, 20 Co-op). **[I]**: setting char type to 0 on the ghost gives the normal look; the Sekiro enum itself was not found. In ER, the enum is different (Local 0, WhitePhantom 1, ... ) so do not transfer ER numbers.
- Animating: no public Sekiro-specific animate-a-puppet method either (see `WEB-ANIM-RESEARCH.md`). New hint: the engine's own remote players are animated through a Network manipulator, and enemies through NetAi (3.1), i.e. by replicated state, not by forcing animation ids.

### (b) Has anyone made host-driven enemies work in Sekiro? How are enemy hits on the remote player handled?

- Working: only Sekiro Online (closed, buggy), via the native sync. Open code: sekiro-coop syncs host enemy HP and pins state at 5 Hz, but the client's enemies are not puppeted (no AI-off).
- What the engine does natively (public DSR/ER facts, DS3 lineage): enemy position and health are continuously exchanged by every player; AI state travels host->guest after join and guest->host; an EnemyAction packet carries attack/idle/death behaviours; the receive side buffers per ChrIns slot a **placement update** (position and rotation in map space) and a **health update** (`current_hp` plus `damage_taken` since last sync) with per-slot update flags, and the NetAi manipulator consumes them and sets `net_position_synchronized` (ER `NetChrSetSync`, `ChrSyncHealthUpdate`, `ChrSyncPlacementUpdate`).
- The puppet is not "AI turned off + pinned". It is a **different manipulator** driving the same ChrIns (3.1).
- Damage direction: in the native model, damage to a player is resolved on the machine of the attacker and sent per damage instance (`PvPDamage`, "once per damage instance", i-frames re-checked by the receiver). Enemy hits on a remote player are not described in the wiki ("PvEDamage" listed without details). Forum anecdotes (DS3/Remnant) say the victim's machine decides whether a dodge succeeds. All of this is [S]/inference. For our design: let hit boxes fire on the machine of the VICTIM from the replicated animation (TAE events spawn the hit boxes), do not synthesize damage packets; send our own HP in the player snapshot as the native `PlayerFrameUpdate` does (position, rotation, HP, ezstate every 10 frames).
- Sekiro-specific cautions from the tracker: on-hit SpEffects (burn from Living Force) were not networked (#14), projectiles/bullets take a different path (#19), roll hitboxes can register as hits (#16).

### (c) Public list of Sekiro event flags for bosses/minibosses/doors/idols and storage layout?

- Bosses/minibosses: the ElaDiDu CT "Bosses" list and SekiroTool `GameEvent.cs` (already tabulated in `WEB-WORLD-AI-RESEARCH.md` 2.3). SoulSplitter has a `Boss` enum for the 93xx ones.
- **Idols (new, two sources agree: SekiroTool `IdolEventFlags` resource and the CT "Unlock All Idols")**: each idol has two flags: `base` = `<map flag prefix>00nn` (unlocked) and `base + 10` (the second state, e.g. visited/lit). Lists (flag -> idol): Hirata `11000000` Dragonspring, `11000001` Estate Path, `11000002` Bamboo Thicket Slope, `11000003` Main Hall, `11000005` Audience Chamber, `11000004` Hidden Temple; Outskirts `11100000` Dilapidated Temple, `11100006` Ashina Outskirts, `11100001` Gate Path, `11100002` Stairway, `11100003` Underbridge Valley, `11100004` Castle Gate Fortress, `11100005` Castle Gate, `11100007` Flames of Hatred; Castle `11110000` Ashina Castle, `11110001` Antechamber, `11110007` Dojo, `11110002` Lookout, `11110003` Kuro's Room, `11110006` Old Grave, `11110004` Great Serpent Shrine, `11110005` Abandoned Dungeon Entrance; Reservoir `11120001` Reservoir, `11120000` Near Secret Passage; Dungeon `11300000`, `11300001`; Depths `11700005` Ashina Depths, `11700004` Poison Pool, `11700006` Ape's Burrow; Mibu `11500000` Hidden Forest, `11500001` Mibu Village, `11500002` Water Mill, `11500003` Wedding Cave Door; Sunken Valley `11700007` Under-Shrine, `11700000` Sunken Valley, `11700001` Gun Fort, `11700002` Riven Cave, `11700008` Bodhisattva, `11700003` Ape's Watering Hole; Senpou `12000000..12000006`; Fountainhead `12500000` Palace, `12500001` Vermilion Bridge, `12500006` Mibu Manor, `12500002` Flower Viewing Stage, `12500003` Great Sakura, `12500004` Palace Grounds, `12500007` Feeding Grounds, `12500008` Near Pot Noble, `12500005` Sanctuary. The "+10" copies (`11000010`...) are the second flag. Idol warp ids (for the Warp function) are different numbers (`1101950`...). The earlier doc's `12500015/17/18` are the +10 flags.
- Doors: nothing public as a list. Tracker #12 says boss-room doors and cutscene events do not sync on their own. Door/fog states live in per-map event flags (`m11_..` scripts) and many are "re-asserted every frame" (sekiro-coop).
- Engine-synced flags: EMEVD `Set Network-Connected Event Flag` and the batch form mark a flag range as network-connected; the engine broadcasts those inside a native session. We have no list of which Sekiro flags are in that range; sekiro-coop's gap list says "runtime probe: set a flag, observe whether it syncs, binary-search the boundary".
- Storage layout: 1.7 above (fromsoftware-rs) plus the SoulSplitter-derived formula in `WEB-WORLD-AI-RESEARCH.md` 2.2. They agree.

### 3.1 The manipulator system (largest new lead)

ER's SDK documents `ChrManipulator` with a `manipulator_type()` virtual returning: Default 0, **Pad 1** (local human), **Network 2** (remote player), Replay 3 (ghost replay), **NetAi 4** (remote-driven enemy), **Com 5** (AI-driven enemy), Ride 6, Follow 7. `ER EnemyIns` has separate pointers to its Com and NetAi manipulators, and its comment says `net_position_synchronized` is "set by NetAIManipulator after receiving a position update". DS3 `PlayerIns` carries a `net_ai_manipulator` too. In Sekiro the same slot is `chr+0x58` (SekiroTool calls it ComManipulator because for enemies it is one; the AI think object is `+0x340` inside it), and the debug "Control Enemy" feature swaps another manipulator in through the `chr+0x50` controller object.
- **[I]** On a client, the engine's own way of making an enemy follow the host is: keep the ChrIns, swap its active manipulator from Com to NetAi, and feed the NetAi object the placement/health buffers. That is the replacement for "AllNoUpdateAi or FreezeAi + position pin" and explains why that combination fights the animation graph (AI and graph both think they own movement).
- Cheap test that needs no new RE: with the existing RTTI dumper, print the RTTI name of `*(chr+0x58)` for (a) the local player, (b) a live enemy, (c) the ghost. Expect Pad / Com / something. Then dump 0x40 bytes after the ChrIns end for an enemy (`chr+0x2000..0x2020`) looking for two more manipulator pointers (Com/NetAi, as in ER's `EnemyIns`).

---

## 4. Sekiro 1.06 offsets collected (RVA unless "off")

Everything below comes from SekiroTool unless marked. Hook-site RVAs are instruction addresses, not function starts. The earlier docs' values are repeated only where a new meaning was added.

### 4.1 Singletons / globals (1.06)
| Name | RVA | Notes |
|---|---|---|
| WorldChrMan | `0x3D7A1E0` | player ChrIns `+0x88`; player ChrSet entries `+0x48` [I: capacity `+0x40`]; ghost set `+0x58`/`+0x60` [I] |
| WorldChrManDbg | `0x3D7A388` | `+0x6F` draw-enable; `+0x150`, `+0x160` debug controller (CT) |
| WorldAiMan | `0x3D55070` | `+0x4C6EC` GlobalForceAct, `+0x4C6ED` GlobalForceKengekiAct |
| GameMan | `0x3D5C0D0` | CT symbol |
| FieldArea | `0x3D5C0A0` | `+0x20` GameRend, `+0x28` current world block index, `+0x30` ChrCam |
| EventFlagMan | `0x3D55FE8` | layout in 1.7 |
| MenuMan | `0x3D67408` | `+0x20` **loaded byte (1 in world, 0 on loading screen)**, `+0x23C` quitout, `+0x1FB0` dialog manager |
| DebugFlags base | `0x3D7A366` (+index) | `+0xB` AllNoDeath, `+0xC` NoDamage, `+0xD` NoHit, `+0xE` NoAttack, `+0xF` NoMove, `+0x10` DisableAi, `+0x17` NoPosture |
| GameDataMan | `0x3D5AAC0` | |
| FPS | `0x3E9F748` (`+0x2BC`) | libsekiro |
| IGT | same pointer as GameDataMan (libsekiro `igt` = `0x3D5AAC0`) | in-game time ms at `+0x9C` |

### 4.2 Functions (1.06)
| Name | RVA | Calling shape / note |
|---|---|---|
| GetEvent / SetEvent | `0x6C3E60` / `0x6C4520` | `(man, id)` / `(man, id, value, 0)` |
| GetChrInsByEntityId | `0x6C1A10` | `rcx = entity id -> rax = ChrIns*` (SekiroTool stub) |
| **GetChrInsWithHandle** | `0xA49F90` | handle (u32 from `ChrIns+0x08`) -> ChrIns. Lets the registry store handles, not raw pointers |
| **ApplySpEffect** | `0xA01720` | `rcx = ChrIns*, edx = SpEffect id` |
| **RemoveSpEffect** | `0xBFB6D0` | `rcx = [ChrIns+0x11D0]` (SpEffect manager), `edx = id` |
| ForceAnimation | `0x6D5A90` | `ecx = entity id, edx = anim id` (from the snake-loop stub) |
| ForceAnimationByChrEventModule | `0xBD84C0` | same RVA family as the HP/posture hooks (ChrData/Event code) |
| Warp | `0x68E870` | `rdx = idol id, r8d = 0` (rcx unused in the stub) |
| MapItemMan GiveItem | `0x91C970` | |
| FrpgCastRay | `0x94CC50` | |
| EzStateExternalEventTempCtor | `0x1C17660` | |
| register_task | `0x11DF640` | fromsoftware-rs |
| Remove item (Lua event man) | `0x67B230` | fromsoftware-rs |

### 4.3 Hook sites (instruction RVAs) and what registers hold
| Name | RVA | What the injected asm tells us |
|---|---|---|
| **HpWrite** | `0xBD651E` | replaces `mov [rbx+0x130], eax`. `rbx` = ChrData module, `eax` = new HP. SekiroTool's NoDeath compares `rbx` with the player's `[chr+0x1FF8]+0x18` and clamps. **The single place HP is stored.** |
| FreezeTargetPosture | `0xBD677B` | posture write in the same module code |
| InfinitePoise | `0xB6C011` | |
| **DamageMultiplier** | `0xB6E076` | after `cvttss2si eax, xmm9` (final integer damage in `eax`); `r15` = per-hit context, `[r15+0x28] == 5` is skipped, `[r15+0x190]` compared with the player ChrIns (attacker side) |
| DamageMultiplierDeflect | `0x842943` | `rdi` compared with the player (victim side), `[rsp+0xD0]` flag |
| **UpdateCoords** | `0xBC4796` | `rsi` = physics module (`[rsi+0x8]` owner ChrIns, `[rsi+0x80]` position). NoClip rewrites the position here for the player. |
| SetWarpCoordinates / SetWarpAngle | `0x832A3A` / `0x832A1A` | write `[rax+0xAC0]` (pos xyzw) and `[rax+0xAD0]` (angle) of the pending-warp struct |
| AddSubGoal | `0x5CE510` | AI goal queue |
| SetLastAct | `0x5E209D` | AI last act byte |
| AiHasSpEffect | `0x6136EB` | |
| LockedTarget | `0x9C3ADA` | |

### 4.4 AOBs worth reusing (from sekiro-coop `aob.rs`, all labelled for 1.0x; valid because they are version-independent patterns)
- WorldChrMan `48 8B C6 48 89 05 ?? ?? ?? ?? 48 85 C0` (rip-rel at +6, len 10); WorldChrManDbg `49 8B C4 48 89 05 ?? ?? ?? ?? 48 8B CF`; WorldAiManager `F3 0F 58 C6 0F 2F F8 ?? ?? 48 8B 0D ?? ?? ?? ?? 48 85 C9`; GameMan `66 0F 42 C8 66 3B CE ?? ?? 48 8B 05`; FieldArea `48 3B C7 48 0F 44 C5 48 89 05`; EventFlagMan `41 8B F5 90 48 8B 0D ?? ?? ?? ?? 48 85 C9`; LockTgtMan `48 8B 35 ?? ?? ?? ?? 49 8B D7 49 8B CE`; DamageManagement `48 8B 1D ?? ?? ?? ?? 48 8B F0 48 85 DB`; MapItemMan `45 33 C0 33 D2 48 8B 0D`; RendMan `48 8B 1D ?? ?? ?? ?? 48 8B 5B 30 48 63 43 20 48 8B 4C C3 10`.
- Functions (match then add offset): SetFlag `45 0F B6 E1 45 0F B6 E8 44 8B F2 48 8B E9` (-19), GetFlag `8B DA 74 ?? E8 ?? ?? ?? ?? 4C 8B C0 48 85 C0 74 ?? B8 D3 4D 62 10` (-13), **ApplyEffect** `44 89 70 9C 45 33 C0 4C 89 70 A0` (-107), SpecialEffectDeleteEffect `48 83 EC 28 8B C2 48 8B 51 08 48 85 D2 74 ?? 90 39 42 58` (0), GiveItem `48 89 58 18 0F 29 70 B8 45 33 ED 44 89 6C 24 44` (-48), WarpNextStage_Bonfire `85 D2 ?? ?? ?? ?? ?? ?? B8 89 B5 F8 14 F7 EB` (-25), AddExperience `48 89 58 F0 48 8B D9 48 89 68 E8 33 ED 48 89 78 E0 41 8D 0C 10` (-15), "EnlargeUnkHkbArray" `C1 E2 04 48 8B 48 58 48 8B 01 FF 50 08 48 8B E8 48 85 C0` (-63; reads `[chr+0x58]`'s vtable slot 1, an hkb related allocation) [unverified names].
- The `damage_management` symbol (`DamageManager.Base`, `+0x30` hitbox view) is a global; its role in hit processing is unknown.

### 4.5 ChrIns / module offsets (1.06)
- ChrIns: `+0x08` handle (u32), `+0x30` -> NPC info (`+0x8` name, `+0x624` ThinkId, `+0x628` NpcId), `+0x50` controller (`+0x10` ?, `+0x358` manipulator override slot), `+0x58` manipulator (type via vtable; `+0x340` AI think), `+0x68` char id, `+0x70` char type (i32), `+0x74` team type (u8), `+0x11D0` SpEffect manager, `+0x1A24` entity id, `+0x1F40..` bit flags (NoHit 0 bit5, NoAttack 0 bit6, NoMove 0 bit7, FreezeAi 1 bit4, NoGoodsConsume 2 bit4, NoEmblemsConsume 3 bit0), `+0x1FF8` module container, `+0x2000` PlayerGameData (player only).
- Modules: data `+0x18` -> HP `+0x130`, MaxHP `+0x134`, Posture `+0x148`, MaxPosture `+0x14C`, **bit flags `+0x228`: NoDeath bit2, NoDamage bit3, NoPostureConsume bit4**, CurrentBossNode `+0x25C`; resist `+0x20` (poison cur `+0x10`, burn `+0x18`, shock `+0x20`); behavior `+0x28` (AnimationSpeed `+0xD00`); super armor `+0x40` (poise `+0x28`, max `+0x2C`, timer `+0x34`); physics `+0x68` (angle `+0x74`, XYZ `+0x80/84/88`, NoGravity `+0x92D`, owner ChrIns `+0x8`).
- Per-ChrIns invulnerability that keeps the character hittable: data bit flags NoDamage (bit 3). Per-ChrIns "do not attack"/"do not move": `chr+0x1F40` bits (NoAttack/NoMove) -- use these on the PUPPET only if its attacks must not spawn local hit boxes; the opposite (leave attacks enabled) is what lets host-driven attack animations hurt the client's local player.

---

## 5. Native packet model (cross-reference)

Checked the Souls Modding wiki page again for hit/damage/animation packets [V]: PlayerFrameUpdate (type 1) every 10 frames carries position, rotation, HP and the ezstate animation id with a 3-bit sequence number; throws (backstab, parry) are a separate immediate packet because those states have no ezstate id (explains "invisible backstab" bug); PvPDamage (18) is sent once per damage instance (multi-hit = many packets) and i-frames are checked by the receiver; ApplyEffectToPlayer (35) carries on-hit SpEffects (poison resin); NPCSpEffectSync / PCSpEffectSync (34, 76) carry SpEffect id + target handle + timestamp; EnemyAction (33), WorldChrSyncHealth (4), WorldChrSyncLocation (3, distance from default spawn), EnemyInit (39), SpEffectInitAlloc/Apply (58, 59) sync existing effects with remaining time to a joiner; ObjDestroy (40) once per breakable object. Lessons: (1) the native design has a dedicated reliable-ish channel for "state that has no animation id" (throws) and for SpEffects. We have hkb events only; throws/backstabs/deathblows (Sekiro's main mechanic) are likely to be this kind. (2) Send HP and animation in the same periodic packet.

---

## 6. Lessons across projects (consolidated)

1. Nobody open has solved remote-character spawn without the native session. Plan on either hijacking the session spawn path or on a cheap stand-in; do not expect a published solution.
2. Position must be written inside the engine's own update window (NoClip's hook), or through Warp. Raw out-of-band writes are silently discarded.
3. Event flags are the dominant desync source in every mod's changelog: leakage, lingering between reloads, flipped on transit, item-pickup flags that convert items in the other world (tracker #15). Whitelist; clear the replicated set on session end; edge-trigger; suppress echo; reliable queue not latest-wins (tracker #2/#3).
4. Two brains (client AI + host AI) is the classic failure (ERSC chariots, DS2 alerted enemies, DS3 hit-detect anecdotes). The engine's solution is a swapped manipulator, not a stopped AI.
5. Culling and streaming hide remote characters at fixed places (tracker #21); equipment textures can fail to load (#13). Test the ghost at Sunken Valley start, the Outskirts snake and Bodhisattva Valley.
6. Hit reactions and posture breaks are the first things to desync under latency (#22). Resend state, not only events.
7. Debug/test: a peer-simulator on one machine (sekiro-coop) and fake jitter/loss (`NETCODE-RESEARCH.md`) beat waiting for testers.

## 7. Gaps (not found)

- How Sekiro Online spawns remote players and sets their look (closed; not examined).
- Which Sekiro event flags are "network-connected" (engine-synced range).
- Sekiro CHAR_TYPE enum values (only DS3's list is public; ER's differs).
- Sekiro `WorldBlockChr` / loaded block list offsets (only DS3's struct is public; my +8 shift is inference).
- Any public door flag list.
- NetAi / Network manipulator constructors or offsets in Sekiro.
- Public description of what `damage_management` (global) does, and the `EnlargeUnkHkbArray` function's purpose.
- Nexus mod page and Patreon text (403).

## 8. Sources

- https://github.com/mstampfli/sekiro-coop (README, docs/HANDOFF.md, docs/GAPS.md, crates/...)
- https://github.com/sekiro-online/sekiro-online (issues 1-22), https://github.com/sekiro-online/params
- https://www.nexusmods.com/sekiro/mods/577 (via search; direct fetch 403), Kotaku/PC Gamer/Siliconera press pieces
- https://steamcommunity.com/app/814380/discussions/0/3271313489436304011
- https://github.com/SanguineLoki700/sekiro-seamless-coop (README, docs/RESEARCH.md)
- https://github.com/borgCode/SekiroTool (Memory/Offsets.cs, Resources.resx, Services/*)
- https://github.com/ElaDiDu/Sekiro-Practice-CT (Ela_Sekiro_Table.CT)
- https://github.com/veeenu/sekiro-practice-tool (lib/libsekiro)
- https://github.com/vswarte/fromsoftware-rs (crates/sekiro, darksouls3, eldenring: world_chr_man, chr_manipulator, net_chr_sync, event_flag)
- https://github.com/AinTunez/DarkScript3 (sekiro-common.emedf.json)
- https://github.com/micthiesen/unseamless-coop (README, docs/ROADMAP.md, SCALING.md)
- https://github.com/himingal/des-seamless-coop
- https://github.com/yuiamoroll/Sekiro-Debug-Patch
- https://soulsmodding.com/doku.php?id=topic%3Adsr_gamedata_communication
- https://github.com/The-Grand-Archives/Dark-Souls-III-CT-TGA (DS3 CHAR_TYPE / TEAM_TYPE dropdowns, copy in scratch `web\ds3tga.ct`)
- DS3/DS2 Seamless public descriptions (Nexus/Patreon via search summaries)
