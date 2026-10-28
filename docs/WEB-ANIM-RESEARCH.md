# Web research: playing and reading animations on a ChrIns (Sekiro 1.06 ReplayGhostIns)

Date: 2026-10-22. Public sources only. No closed binaries were read or downloaded. Three public text files were fetched as data (SekiroTool `Resources.resx`, the DS3 TGA `.CT` XML, the DarkScript3 Sekiro EMEDF JSON) and only grepped, never loaded in Cheat Engine or run.

Reading guide. All offsets are hex. "Container" means the module-pointer table reached from ChrIns (`+0x1FF8` in Sekiro 1.06 per SekiroTool, `+0x1F90` in DS3, `+0x190` in ER). Confidence is about how likely the item applies to Sekiro 1.06 ReplayGhostIns, not about whether the source is right.

## 0. Key structural facts (cross-game)

### 0.1 The DS3 module container matches Sekiro's slot order

Sources:
- fromsoftware-rs DS3: https://github.com/vswarte/fromsoftware-rs/blob/main/crates/darksouls3/src/sprj/chr_module.rs (DS3, names "copied from ER" and RTTI)
- SekiroTool Offsets: https://github.com/borgCode/SekiroTool/blob/master/SekiroTool/Memory/Offsets.cs (Sekiro 1.06)
- fromsoftware-rs ER: https://github.com/vswarte/fromsoftware-rs/blob/main/crates/eldenring/src/cs/chr_ins/module.rs

DS3 container slots (8 bytes each): `0x00` action_flag, `0x08` behavior_script, `0x10` time_act, `0x18` data, `0x20` resist, `0x28` behavior, `0x30` behavior_sync, `0x38` ai, `0x40` super_armor, `0x48` toughness, `0x50` talk, `0x58` **event**, `0x60` magic, `0x68` physics, `0x70` fall, `0x78` ladder, `0x80` action_request, `0x88` throw, ... `0xB8` behavior_data.

SekiroTool's Sekiro 1.06 slots are `0x18` data, `0x20` resist, `0x28` behavior, `0x40` super armor, `0x68` physics. Our own notes say time act is `0x10`. All of these match the DS3 table exactly. So the DS3 slot list is very likely valid for Sekiro, including event = `0x58`, behavior_sync = `0x30`, behavior_script = `0x08`, action_request = `0x80`.

ER's container is shifted (data `0x00`, time_act `0x18`, event `0x58`, behavior `0x28`). Do not use ER slot numbers for Sekiro, only ER's inner struct layouts.

Applicability to Sekiro ReplayGhostIns: confidence high (about 80%) for the slot map. Action: dump `container[0..0x40]` of the ghost and the local player and diff which slots are null. Our NULL event module for the ghost is probably just `container[0x58] == 0`, which would be a real property of ReplayGhostIns and not a bug in the slot 247 route.

Note: fromsoftware-rs has a `sekiro` crate but it has no `chr_ins` file (only fd4, sprj menu/params/event flags, etc.). No Sekiro ChrIns structs exist there. DS3's crate does define `ReplayGhostIns` as a plain subclass of `ChrIns` with no extra fields (same file family: https://github.com/vswarte/fromsoftware-rs/blob/main/crates/darksouls3/src/sprj/chr_ins.rs). In DS3, ChrIns is 0x1FA0 bytes and `modules` sits at 0x1F90.

## 1. Playing animations

### 1.1 CSChrEventModule has a "request animation id" field (best explanation of EMEVD ForcePlayAnimation)

- Source: https://github.com/vswarte/fromsoftware-rs/blob/main/crates/eldenring/src/cs/chr_ins/module/event.rs (ER)
- Says: `CSChrEventModule` (size 0x78) has `owner` (ChrIns*) at +0x08, **`request_animation_id` (i32) at +0x18** documented as "override animation ID to play on the next frame", `idle_anim_id` at +0x1C, flags at +0x40 (bit 1 = i-frames).
- Corroboration, ER: The Grand Archives ER Cheat Engine table, entry "Idle Animation": path `WorldChrMan -> [main player at +0x10EF8] -> +0x190 (modules) -> +0x58 (event module) -> +0x18` (4 bytes). Source: https://github.com/The-Grand-Archives/Elden-Ring-CT-TGA (file `CheatTable/CheatEntries/Open - The Grand Archives - Elden Ring/Hero/Animation/Idle Animation.xml`). FearLess thread https://fearlessrevolution.com/viewtopic.php?t=25800 describes the same: you write an animation id, it moves into "Current Animation" on the next opportunity, and the field resets to -1.
- Corroboration, DS3: TGA DS3 table, "Animation Override" entry and a Lua revive script use `[[[[WorldChrMan]+80]+1F90]+58]+20`, so DS3 has the same field at **+0x20** in its event module (the DS3 module is 8 bytes bigger before it). Source: https://github.com/The-Grand-Archives/Dark-Souls-III-CT-TGA (file `DS3_The-Grand-Archives.CT`, entry "Idle Animation" id 1337081653 and a script near "reviveAnimation = 63000").
- Sekiro: the event module is probably `container[0x58]`. The request field is therefore likely at +0x18 or +0x20 of that module. Our `vtbl[7](module, anim)` is probably the setter for this field plus some state.
- Why it matters: this is almost certainly the data the EMEVD route ends up writing. But the ghost has no event module (slot probably null), so it cannot be used as is.
- Applicability: high that this is the right mechanism for normal characters; low that it helps the ghost directly.
- Fallback idea (low confidence, risky): build a synthetic 0x78-byte event module with `owner = ghost` and put it into slot 0x58. The ER struct is tiny and mostly flags, but the consumer reads other state from the real module, and the ghost's update code may never call it. Only try after finding who reads `request_animation_id` (see 1.5).

### 1.2 DS3: play an animation by sending an hkb event to hkbCharacter (strong lead, works on any character with a behavior graph)

- Source A: TGA DS3 table script "PlayAnimation" in https://github.com/The-Grand-Archives/Dark-Souls-III-CT-TGA (`DS3_The-Grand-Archives.CT`, entry id 1337261261, ~line 38530 of the file).
- Says: finds a function with the AOB `74 ?? 48 85 d2 74 ?? 48 8d 4c 24 50`, takes the address **minus 0x0F** as `W_Event`, then calls it as `W_Event(hkbCharacter, const char* eventName)`. It returns 0xFFFFFFFF on failure (unknown event name). The hkbCharacter handle is `[[[[[WorldChrMan]+80]+1F90]+28]+10]+28`, i.e. ChrIns -> container -> **behavior module (slot 0x28) -> +0x10 -> +0x28**.
- The table ships a list of about 500 hkb event names for c0000 (the player) such as `W_Event14000`, `W_Event60000`..., `W_Attack3000`. So many animations are played by event name `W_Event<animId>`, not by raw animation id. The named events must exist in the character's HKB graph.
- Source B: DS3RuntimeScripting (AmySouls): https://github.com/AmySouls/DS3RuntimeScripting/blob/master/ds3runtime/chr_ins.cpp (DS3 1.15.x)
  - `playAnimation(int id)`: builds `int32_t input[6] = { id, 0, 0, ... }` and calls `fn(hkbCharacter, input)` where `fn = DarkSoulsIII.exe+0xD84870` (RVA-style `0x140D84870`). The handle comes from the same chain plus `+0xB8`: `[[[[[ChrIns+1F90]+28]+10]+28]+B8]`.
  - `playAnimation(wstring)`: same `...+28]+10]+28` object, calls `0x140D84450` with a wide string (name path).
  - `getHkbIdFromString` / `getHkbStringFromId`: `0x141049BD0` / `0x141049BF0`, first arg is the object at `...+28]+10]+28]+A0` (the string/event table).
  - Docs in that repo (chr_ins.h) say playAnimation is relayed for the local player and desyncs for others; it is just a local hkb call.
- Applicability to Sekiro: the chain `container[0x28] -> +0x10 -> +0x28` is probably the same wrapper layout (Sekiro's behavior module is at slot 0x28 per SekiroTool; the inner offsets may have moved). The function itself will have a different address. We need to find Sekiro's version: it is the function that takes (hkb character wrapper, event name or id) and calls `hkbCharacter::notifyEvent`-style code. Practical ways without any closed-binary work: search our own Sekiro 1.06 dump for string literals of event names ("W_Event", "W_Attack") xrefs, or search for a function that does a string-to-event-id lookup in the string table then queues the event.
- Confidence this applies to the ghost: medium. The ghost uses the player model and (if it uses c0000's behavior) has an hkbCharacter. Check `container[0x28] != 0` on the ghost first. The anim ids must be mapped to existing event names. For Sekiro the event names for c0000 can be listed from the game's unpacked `c0000.behbnd` using any public HKX tools (DSAnimStudio, DSMapStudio, HKXPack-style) — not done here.
- Unverified inference: whether Sekiro's EMEVD `Force Animation Playback` goes through the same `W_Event<id>` events. The DS3 table list shows `W_Event<id>` events are common, which fits.

### 1.3 Debug "Test Animation" block inside the behavior module

- Source: DS3 TGA `.CT` (same file), section "Animation" / "Debug Animation" near lines 25480-25900.
- Says (DS3 behavior module = container[0x28]): `+0xA58` PlaySpeed (float), `+0xA5C` **TestAnimId** (int), `+0xA60` TestAnimLoop (byte), `+0xA61` Pause (byte), `+0xA62` FramePlay (byte), `+0x898` current animation name (UTF-16 string, 20 chars), `+0xA58` also what DS3RuntimeScripting calls the animation speed.
- A second debug path ("アニメテスト[animId]" = anime test): `ChrIns+0x50 -> +0x10`, with `+0xF8` = test anim id, `+0xFC` = loop flag, and a "play" script that calls `DarkSoulsIII.exe+0x8150B0` with `rcx = [[ChrIns+50]+10]`, `rdx = pointer to a 0x20-byte data buffer (dd 0x1000 at +0x18)`, `r8d = 0x14`. Names suggest ChrIns+0x50 is the player control object. Not obviously usable for non-player characters.
- ER: `chr_behavior_debug_anim_helper` at behavior module `+0x17B0`, `animation_speed` f32 at `+0x17C8` (fromsoftware-rs `behavior.rs`: https://github.com/vswarte/fromsoftware-rs/blob/main/crates/eldenring/src/cs/chr_ins/module/behavior.rs; TGA ER "Animation Speed" entry confirms `container[0x28]+0x17C8`).
- Sekiro: SekiroTool has `ChrBehaviorOffsets.AnimationSpeed = 0xD00` (module base presumably `container[0x28]`). By analogy with DS3 (speed at A58, TestAnimId at A5C, loop at A60), the Sekiro test fields are probably speed `0xD00`, TestAnimId `0xD04`, TestAnimLoop `0xD08`, Pause `0xD09`, FramePlay `0xD0A`. This is a guess from layout analogy only.
- Applicability: low-medium. Writing TestAnimId alone probably does nothing without the game's debug code path running; but this block is a cheap thing to watch/diff on the ghost vs player while the player animates.

### 1.4 Sekiro's own ForceAnimation functions (existing public RVAs and calling shape)

- Source: https://github.com/borgCode/SekiroTool/blob/master/SekiroTool/Memory/Offsets.cs and the embedded script `SnakeCanyonIntroAnimationLoop` in https://github.com/borgCode/SekiroTool/blob/master/SekiroTool/Resources.resx
- Offsets (RVA from module base), Sekiro 1.06 (SekiroTool "1.6.0"): `Functions.ForceAnimation = 0x6D5A90` (same as 1.05), `Functions.ForceAnimationByChrEventModule = 0xBD84C0` (same as 1.05). `Functions.EzStateExternalEventTempCtor = 0x1C17660`. `WorldChrMan.Base = 0x3D7A1E0`. (These agree with the ones already in `RE-FINDINGS-2.md`.)
- Calling shape: the script's loop does `mov ecx,0x10CC32 ; mov edx,0x4E20 ; call ForceAnimation`. So ForceAnimation takes at least two 32-bit args: `ecx = 0x10CC32 (1,100,850)` and `edx = 0x4E20 (20000)`. By the EMEVD signature (entity id, animation id, ...) the first is almost certainly an **entity id** and the second an **animation id**. It is a 5-byte thunk, so the real arg list (loop, wait flags) lives in the target. Inferred, not confirmed.
- EMEVD side (DarkScript3 Sekiro EMEDF): https://github.com/AinTunez/DarkScript3/blob/master/DarkScript3/Resources/sekiro-common.emedf.json
  - `2003[1] Request Animation Playback`: Entity ID, Animation ID, Should Loop, Should Wait For Completion, Comparison Type, Number of Target Entities.
  - `2003[18] Force Animation Playback`: Entity ID, Animation ID, Should Loop, Should Wait For Completion, Ignore Wait For Transition, unk, unk.
  - `2004[39] Set Character Animation State`: Entity ID, Disabled/Enabled. `2004[43] Request Character Animation Reset`: Entity ID, Interpolation Method. `2004[9] Set Special Animation(s)` (waiting/damage/cancel/death/return animations).
  - Two distinct instructions exist (Request vs Force). Force has "Ignore Wait For Transition", which suggests it bypasses the normal HKS-side state gating.
- Applicability: these resolve a character by entity id. A ghost spawned by us has no entity id, which could explain failures even before the event module step. If you want to keep this route, give the ghost a unique entity id (ChrIns `+0x1A24` is EntityId per SekiroTool) and test `ForceAnimation(entityId, anim)` again. Medium-low.
- Note: the `Set Character Animation State` and `Request Character Animation Reset` instructions are worth checking for side effects on the ghost: if the ghost's animation is "disabled", requests may be dropped.

### 1.5 behavior_sync and behavior_script modules (possible native mirroring path)

- Source: the DS3 container list in fromsoftware-rs `chr_module.rs` (see 0.1). Names (`behavior_sync`, `behavior_script`) are from ER and unconfirmed for DS3/Sekiro; no public code documents what they do.
- Reasoning only: a "behavior sync" module is the natural place where networked (remote) players get their hkb variables and events replicated, and ReplayGhostIns (replay of recorded animation) must also drive its graph from stored data. In DS3, bloodstain replays are described as "serialized animations" in `replay_data`, sampled over time (https://timleonard.uk/2022/06/18/reverse-engineering-dark-souls-3-networking-part-6; the Demon's Souls PS3 decompile note https://github.com/JonathanDC64/DemonsSoulsRecomp/pull/94 says ghosts step to the next stored sample every 10 frames, with a `ReplayManipulator` class). So the engine's own ghost animation is probably fed from replay data through a Manipulator (the ghost's pad/manipulator object) plus the sync module, not through the event module. This fits why the event module is null on the ghost.
- Action (cheap, no closed code): read the RTTI/vtable name at `ghost.container[0x30]` and `[0x08]`, compare to the local player and to a ReplayGhostIns, and look at ChrIns `+0x58` (SekiroTool names it ComManipulator) on the ghost. If the ghost's manipulator is a "ReplayManipulator", the supported way to animate it may be to feed that manipulator (a recorded-sample structure) rather than to force animations.
- Confidence: speculative (about 30%) but cheap to check.

### 1.6 What public projects do to make a puppet follow another player's animation

No public open-source project was found that documents mirroring another player's animation onto a puppet (searched GitHub-style topics for ER/DS3/Sekiro co-op). Closest practical equivalents found:
- TGA ER/DS3 tables and DS3RuntimeScripting simply use the two methods above: write `request_animation_id` / idle id (event module), or fire an hkb event by name/id into hkbCharacter. DS3RuntimeScripting marks both as "relay for self, desync for other players", meaning no native animation sync exists for them either.
- `ds3os` (https://github.com/tleonard/ds3os, per Tim Leonard's series) is a server reimplementation only; the replay blob is opaque to it.
- LukeYui's closed seamless co-op was not examined, as instructed.

## 2. Reading the current animation id reliably

### 2.1 Entry 0 of the TimeAct queue (not the ring head)

- Sources: TGA ER table "Current Animation" (id 100970): chain `WorldChrMan -> main player -> +0x190 -> +0x18 (time_act) -> +0x20` (4-byte anim id) and "Length played [seconds]" at `+0x24` (float). DS3 TGA table "Animation Time"/"Max Animation Time": `container[0x10] +0x24` / `+0x2C` (floats). DS3RuntimeScripting `getAnimationTime`/`getMaxAnimationTime` read the same, and its `animation_id_handler.cpp` polls all 10 slots at `timeact + 0x20 + i*0x10` and reports the slot whose value changed last. Sources: https://github.com/vswarte/fromsoftware-rs/blob/main/crates/eldenring/src/cs/chr_ins/module/time_act.rs , https://github.com/AmySouls/DS3RuntimeScripting/blob/master/ds3runtime/scripts/animation_id_handler.cpp
- fromsoftware-rs ER struct `CSChrTimeActModule` (size 0xD8): `anim_queue[10]` of 0x10-byte entries at `+0x20` (`anim_id` i32, `play_time` f32, `play_time2` f32, `anim_length` f32), then `write_idx` u32 at `+0xC0` ("index of the next animation to play or update") and `read_idx` u32 at `+0xC4` ("index of the last animation played or updated").
- Mapping to Sekiro (our measured layout): entries are 0x14 bytes (id, start, end, duration, start frame), 10 of them = 0xC8, ring starts at +0x20, so the end of the ring is +0xE8. That is exactly where we read the "head". By analogy to ER (write_idx at ring_end, read_idx right after), **Sekiro likely has write_idx at +0xE8 and read_idx at +0xEC**.
- Takeaways: (a) the tables that work in practice read entry 0, which suggests the game normally keeps the active animation in slot 0, with the other slots being queued or blended ones; (b) the correct "currently playing" slot may be `read_idx`, not `(head+9)%10`; (c) the float at +0x24 (play time) is a good "is it advancing" signal; compare with duration at +0x2C (Sekiro entry field `+0xC`).
- Action: log, per frame, the whole ring plus `+0xE8` and `+0xEC` while the player does roll/idle/attack, and see which slot tracks the on-screen animation. Prefer `entry[read_idx]` if it works, with entry 0 as the fallback. Confidence the layout carries over: high for ring/entry fields, medium for `read_idx` at +0xEC.

### 2.2 Other places a current animation id appears

- DS3 TGA "Current Animation" (id 1337081655): `[WorldChrMan]+80 -> +1F90 -> +80 -> +C8`. So DS3 `container[0x80] + 0xC8` holds the current animation id (container slot 0x80 is the `action_request` slot in fromsoftware-rs). A DS3 TGA assembler snippet also comments `last animation ID Highlited` at this field. Sekiro might have the same at `container[0x80]+0xC8`, but ER's action_request layout has no such field, so this is uncertain; it is a cheap extra read to try.
- DS3 TGA "Current Animation Name": `container[0x28] + 0x898` (UTF-16, hks/hkb animation or event name). Sekiro offset unknown (DS3 +0x898 is just a starting point to search near).

## 3. Other notes

- fromsoftware-rs ER `CSChrActionRequestModule` (size 0x140): `npc_action_id` at +0xF4 is the EzState action id from AI; `requested_gesture` at +0xF8; `queue_tae_id_override` at +0x134. Source: https://github.com/vswarte/fromsoftware-rs/blob/main/crates/eldenring/src/cs/chr_ins/module/action_request.rs . Not about playing animations by id, so low value here.
- ER TGA changelog mentions an `executeEzStateEvent(id, params, chrInsHandle)` helper (by tremwil) and "Idle Animation IDs (summon anims, fog wall)". The EzState route runs a state-machine command on a chr; SekiroTool has the same idea with `EzStateExternalEventTempCtor` (`0x1C17660` on 1.06) and `ExecuteTalkCommand`, but that is the menu/talk EzState, not chr animation EzState. Source: https://github.com/The-Grand-Archives/Elden-Ring-CT-TGA/blob/master/CHANGELOG.md . Low.
- Souls Modding Wiki on HKS: animations are driven by hks code calling `ExecEventAllBody("W_...")` / `ExecEventHalfBlend`, which fire named hkb events (`hkbFireEvent`). That matches 1.2. Source: https://soulsmodding.wikidot.com/topics:havok-behavior-editing

## 4. Ranked leads

1. **Compare the ghost's container to the player's (and find the null slots).** Cheap and decisive. Dump `container[0x00..0x90]` for the ghost and the local player, print the RTTI names. Expect event (0x58) to be null on the ghost and check behavior (0x28), behavior_sync (0x30), behavior_script (0x08), time_act (0x10), action_request (0x80). Based on the DS3 slot map that matches SekiroTool (0.1). Everything below depends on what is non-null.
2. **Fire hkb events at the ghost's hkbCharacter ("W_Event<animId>").** DS3 TGA does exactly this with the AOB `74 ?? 48 85 d2 74 ?? 48 8d 4c 24 50` (-0x0F), `W_Event(hkbCharacter, "W_Event14000")`, handle at `container[0x28] -> +0x10 -> +0x28`. In Sekiro you need to find the equivalent notify-event function and confirm the chain; the ghost needs a non-null behavior module. This needs no event module and no entity id.
3. **Fix the current-animation read: use ring entry 0 and `read_idx` (probably +0xEC), plus the play-time float.** Both TGA tables and DS3RuntimeScripting read entry 0 (`timeact+0x20`, time at +0x24, length at +0x2C of the 0x10-stride layout; ours is the 0x14-stride layout). fromsoftware-rs documents `write_idx`/`read_idx` right after the ring.
4. **Check what the ghost's manipulator/sync modules are.** If the ghost is driven by a replay manipulator and `behavior_sync`, the intended way to animate it is to feed that data. ReplayGhostIns replays recorded "serialized animations" (DS3 network notes) and ghosts step through stored samples (DeS decompile notes).
5. **Event module `request_animation_id` (ER +0x18, DS3 +0x20; Sekiro probably one of these) plus an entity id.** This is almost certainly what EMEVD ForcePlayAnimation writes. It only helps if the ghost has an event module or we build a synthetic one. As a lighter variant, give the ghost an entity id and call SekiroTool's `ForceAnimation` RVA `0x6D5A90` as `(entityId, animId, ...)` (calling shape inferred from the snake loop script: ecx = entity id, edx = animation id).

Not found: any public project that mirrors another player's animation onto a puppet; any Sekiro-specific offset for the behavior module's hkbCharacter chain, the hkb event function, or the event module's request field; any public ReplayGhostIns documentation beyond the DS3 struct stub.

## 5. Source list

- fromsoftware-rs (ER/DS3/Sekiro structs): https://github.com/vswarte/fromsoftware-rs
- SekiroTool (borgCode), Sekiro 1.06 offsets and embedded asm: https://github.com/borgCode/SekiroTool
- DS3RuntimeScripting (AmySouls): https://github.com/AmySouls/DS3RuntimeScripting
- The Grand Archives ER table: https://github.com/The-Grand-Archives/Elden-Ring-CT-TGA
- The Grand Archives DS3 table: https://github.com/The-Grand-Archives/Dark-Souls-III-CT-TGA
- DarkScript3 Sekiro EMEDF: https://github.com/AinTunez/DarkScript3/blob/master/DarkScript3/Resources/sekiro-common.emedf.json
- FearLess thread on the ER idle/current animation fields: https://fearlessrevolution.com/viewtopic.php?t=25800
- Tim Leonard, DS3 networking part 6 (replay_data): https://timleonard.uk/2022/06/18/reverse-engineering-dark-souls-3-networking-part-6
- DemonsSoulsRecomp PR 94 (ReplayManipulator notes): https://github.com/JonathanDC64/DemonsSoulsRecomp/pull/94
- Souls Modding Wiki, Havok Behavior Editing: https://soulsmodding.wikidot.com/topics:havok-behavior-editing
