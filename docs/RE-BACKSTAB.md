# RE: why the client cannot backstab / deathblow NetAI enemies (Sekiro 1.06)

Pass 2026-10-23. Static analysis only (IDA 9.3 headless on a copy of `quick_re7.i64`, job outputs in `F:\Temp\re_throw\o1..o31`). RVAs are relative to the image base `0x140000000`.
Tags: **[VAR]** read from decompiled code, **[ARV]** inference.

## Short answer

1. **No authority veto.** The throw/deathblow path never reads the manipulator, `chr+0x58`, `+0x2000/+0x2008`, `+0x2040`, `forceLocal` or `0x9F33F0` [VAR: every function in the chain below was decompiled]. NetAI does not block the deathblow directly.
2. **What does block it is the defender-side state**, which the game rebuilds every frame from the enemy's own local simulation:
   - the "throwable state" bytes (from TAE events and the enemy's HKS);
   - the action-flag bits (from TAE);
   - SpEffects that the ThrowParam row requires on the defender;
   - HP / "HP-zero" state;
   - geometry and line of sight.

   On a NetAI enemy, the hkb/TAE state is only what the host's sparse behavior records produce (the friend log shows 0-63 written behavior records per 20 s for about 25 enemies). Anything the local AI or its HKS decisions would normally produce is missing or stale. [ARV] Which single gate fails could not be settled statically, because the TAE, HKS and param data live in game archives that are not on disk. The fastest way to settle it is the diagnostic hook in Patch A, step 1.
3. **Even when a throw starts, NetAI breaks it.** The NetAIManipulator keeps moving and snapping the enemy (snap when yaw error > 90°) with no throw gate. Only the hkb-event apply (`0xB4E9C0`) is gated by the throw state [VAR]. A posture break is also undone by the next host behavior record, because nothing gates "posture broken" [ARV].
4. **Fix:** a throw-window handoff to the local AI (Patch A). Then mirror state only if the diagnostic shows it is cheaper (Patch B), and in any case freeze NetAI during an active throw (Patch C).

## a) How a player starts a throw / backstab / deathblow [VAR]

```
0x7D8430 (player ChrCtrl input, call at 0x7D85C4)
  -> 0xBE0E50 SprjChrThrowModule::RequestThrow_AllChr(throwModule, THROW_INPUT_STATE*)
       (name from RTTI of its local ChrFinderImp, vtable 0x2A8EBC0)
```

The module is `ThrowModule = *(chr+0x1FF8)+0x88` (module index 17, ctor `0xBDEB60`, vtable `0x2A8E8B0`).

**`0xBE0E50`, the attacker pre-check.**
- Requires `chr+0x1A14` bit 4 clear and a ThrowModule present.
- Walks the global throw-kind list `*(0x3D78488)+0x278..+0x280`.
- Loads each ThrowKindParam row (`0x1410D0370`) and keeps it if `0xBE3520(attacker, kindRow, input)` passes. That function checks:
  - the input type: 0 = button, with `acceptableButton` at kind `+0x0E`; 1/4 use the hit object at input `+0x10`; 2/3/5 skip this;
  - the attacker's throwable state (ThrowModule `+0x78`, else `+0x79`) through the table `0xBE0950`;
  - the equipment/item check `0xBDFBA0`, and `0xBE4190`.
- It then runs a ChrFinder over all chr sets: `0xA4C260(WorldChrMan, finder, 0)`.
  - Finder slot 1 is `0x9E6B90` (chr-set filter, "without replay"). Slot 2 is `0xBDF930`, the per-chr test.

**`0xBDF930(finder, target)`.** Each of these must hold:
- `0xC09AA0(team(attacker), team(target)) != 0` (hostile). Team comes from `0x9F1510`: `chr+0x74` with SpEffect overrides, no manipulator.
- `chr+0x1FF4 != 1000`.
- **`0x7DCE60(target) == 0`.** With `e = *(chr+0x18)` (chr-set slot entry), this means: `e+0x20` bit 0 clear; if bit 1 is set, then `*(e+0x30)+0x8C` bit 0 must be set (`0x9E5C00`); `e+0x10 == 4`; and finally `chr+0x110 == 0` (int, ctor 0, [ARV] update/omission level).
- `0xBDE5C0(...)` returns true. It requires attacker != target, target `+0x1A14` bit 4 clear, and both ThrowModules. It then calls:
  - **`0xBE2A30`**: collect the candidate throws for this pair (below);
  - **`0xBE2D60`**: select and start one (below).

**`0xBE2A30`, candidate collection.** For each throw kind:
- `0xBE3810(atk, def, kindRow)` must pass (defender-side check, below).
- Then, for each ThrowParam row of that kind: `row[0] == atk chr+0x6C`, `row[1] == def chr+0x6C`, and **`0xBE3A90`** passes.

**`0xBE3810`, defender gates.** ThrowKindParam layout as in `vendor/sekiro` `THROW_KIND_PARAM_ST`. For a normal check (`throwKindCheckMethod` at `+0x0F` == 0):
- Def vtable **slot 170** `0x9F3250(def, 0)` must be false. It returns true if:
  - the chr-set entry is not "active" (same test as `0x7DCE60`);
  - or `chr+0x1A11` bit 0x20 is set;
  - or `chr+0x1A13` bit 2 is set;
  - or `modules[0]+0x70` has bit 0 or bit 1 set.
- `modules[0]+0x70` bits 0x10 and 0x20 must be clear. `modules[0] = **(chr+0x1FF8)`, a u64 of TAE action flags (TAE case `0x397` ORs into it).
- If the def ThrowModule `+0x10` (ThrowNode) is set and `node+0x18` is set but `+0x7A == 0`: reject.
- **Defender throwable state** = ThrowModule `+0x7A`, or `+0x7B` if `+0x7A == 0`. `0xBE0A40(state, kind.category at +0x10)` is a lookup in the table at `0x2A8DE90` and must allow the pair.
- Kind `+0x13` set: the attacker's target in `*(modules+0x68)+0x1C0` must be the defender's handle.
- Kind `+0x15` `defAliveCheck`. "HP-zero" means `0xBD5D40` (HP `*(modules+0x18)+0x130 <= 0` and no revive SpEffect) or `0x84ECB0` (HP == 1 and SpEffect stateInfo 143).
  - 1: the defender must **not** be HP-zero (stealth and posture deathblows).
  - 2: the defender **must** be HP-zero (boss HP deathblow).

**`0xBE3A90`, pair checks.**
- ThrowParam `+0x6C`: SpEffect the attacker must have. ThrowParam `+0x70`: **SpEffect the defender must have**. Both use `0x9F1C80` → `0xBFEFB0`, which walks the SpEffect list `chr+0x11D0`; node `+0x58` is the id, `+0x70` is next.
- `0xBE0270`: distance and angle.
- `0xBE3BA0`: further shape checks.
- `0xBE3DA0`: physics raycast for line of sight. It uses filter `0x8B3540(76)`, or 65 only for a local-vs-remote *player* pair (`0x9F3430` / `0x9F36C0`).

**Where the throwable state comes from.**
- TAE dispatcher `0xB59170`:
  - event **931** (`0x3A3`) writes `ThrowModule+0x78` (attacker);
  - event **932** (`0x3A4`) writes **`+0x7A`** (defender);
  - event 933 calls `0xBE0E40`.
- HKS `act` handler `0xB32410` (registered in `0x1042480`; act-by-id `0xB4E520` → `0xB31F70`):
  - **act 160** writes `+0x79`;
  - **act 161** writes **`+0x7B`**.
- HKS `env` handler: `0xB36410` (`0xB4E590` → `0xB35AF0`).

So the defender's throwability is animation data (TAE) plus the enemy's behavior script (HKS), filtered by the SpEffects that the ThrowParam requires.

**`0xBE2D60`, select and start.**
- **Input matches:**
  - it stores the def handle in attacker ThrowModule `+0x74`;
  - calls the attacker manipulator's slot 18;
  - then calls **`0x802D70(atkNode, defNode, throwId, 0, flag)`** → ThrowMan `0x7FA680`. That registers both ThrowNodes as a pair, writes the ChrCtrl throw flags of both chrs (`0x7DE2C0`), and the pair update `0x7FAB00` drives both animations (hkb `W_ThrowAtk` / `W_ThrowDef`, event prefix table `0x3B151F0[1] = "W_Throw"`).
- **No input yet:** it applies the kind's `noThrowInputSpecialEffectForDef` (kind `+0x04`) to the **defender** through `0x9E9C00`, or refreshes it with `0xC00340`. This is the visible deathblow-marker SpEffect, so **if the red marker appears on the client, availability passed and the failure is in execution.**

## b) What on a NetAI enemy makes this fail

**Checked and clean** [VAR]:
- The finder, `0xBDF930`, `0xBDE5C0`, `0xBE2A30`, `0xBE3810`, `0xBE3A90` and `0xBE2D60` contain no manipulator-type check (vtable slot 2), no `+0x2040` and no `0x9F33F0`.
- The only manipulator uses are notifications, `GetManipulator 0x9EFEA0` (`ChrCtrl+0x358 ?: ChrCtrl+0x120`):
  - slot 18 (`+0x90`) in `0xBE2D60` (attacker) and in ThrowMan `0x7FAB00` (both sides);
  - for NetAI that slot is a nullsub (`0x7E3C50`); for ComManipulator it is the AI notify `0x7E4F90` → `0x607780`.
- Vtable diff: ComManipulator `0x29D2FB8` vs NetAIManipulator `0x29D3EB8`, 46 slots each. In NetAI, slots 3, 4, 13-19 and 42-44 are nullsubs, and 29 and 39 return 0. None of them is read on the throw path.
- `chr+0x1A10` bit 0x20 and `*(ChrCtrl+0x50)+0x250` bit 0x20 are written each frame only by the ComManipulator update `0x7E67A0`, and read by HKS `env(261/264)`. I checked them: they mirror the AI **flying** state (`AI+0x741C`, set by Lua `RequestChangeFlyingState`, `0x608650`), not awareness. Not the cause.

**Remaining candidates**, ranked [ARV]:
1. **Defender SpEffect required by the ThrowParam** (`+0x70`), or HKS act 161, produced by the enemy's AI/HKS from awareness ("not noticed" for stealth). The AI on `ComManipulator+0x340` does not run while NetAI is active (`0x7E67A0` is not called). Host-side SpEffects and AI state are never sent.
2. **TAE throwable state (event 932) / action flags** of an animation the client enemy is not in, because behavior records are sparse and only state diffs.
3. **Posture break:**
   - posture is local and never written by the mod, so the break itself happens;
   - but the stagger animation (and its 932 window) is replaced by the next host behavior record, because `0xB4E9C0` only gates on `0xBDFB70` / `0x802A60`, not on a posture break;
   - and `defAliveCheck` uses HP, which the mod overwrites from the host.
4. **Execution, if the marker appears but the deathblow aborts.** NetAI `0x7EA5C0` still moves the enemy each frame:
   - velocity `manip+0x370` through slot 41;
   - snap flag `manip+0x390` when yaw error > 90°, speed is too high, or the record gap is > 2 s.

   There is no throw gate on either, and a throw turns the defender. Only the hkb-event apply `0xB4E9C0` is gated: `0xBDFB70(modules+0x88)` = ThrowNode, `0x802A60` = `node+0x144 > 0`.

Not a factor: `chr+0x110` and the chr-set entry flags are identical for AI and NetAI as far as read. The slot-37 override does not touch them. Still log them once (A, step 1).

## c) Smallest change, detection and host sync

**Detecting a throw on the client, reliably [VAR]:**
- Defender: `TM = *(*(chr+0x1FF8)+0x88)`; `node = *(TM+0x10)`; throw active if `node != 0` or `*(float*)(node+0x144) > 0`. These are the same tests the game uses in `0xB4E9C0`.
- Attacker (local player): `*(i32*)(TM+0xE8)` is the current/last throw param id (ctor `-1`; [ARV] that it returns to `-1` when idle). `*(u32*)(TM+0x74)` = def handle after `0xBE2D60` [VAR].
- "A throw is available" (marker): the defender has the kind `+0x04` SpEffect, or hook `0xBE2D60` and read its return value (the throw id, `-1` if none).

**Host sync.** The cumulative damage report is enough for the kill. The deathblow damage (`NinsatsuDamage`) lands locally, the client sends the HP drop, and the host's enemy dies with its generic death animation. The host does not need to see the throw (cosmetic only).
- Risk: multi-life enemies (minibosses). A client deathblow should consume one life, but the damage report drives host HP to 0, so the host enemy enters its own HP-zero deathblow state instead. This needs a separate "deathblow" event later [ARV].
- The mod's existing `lethal_hold` and release-on-death rules already cover the client side after the kill.

## d) Host backstab with the friend's native remote nearby

The throw chain has no "remote player nearby" condition [VAR]. The local-vs-remote collision filter in `0xBE3DA0` applies only to player-vs-player pairs. The host loses stealth deathblows only when its own AI notices the friend's native remote PlayerIns (if the AI targets it, the enemy goes to battle). That is the same as vanilla co-op and is expected [ARV].

## PROPOSED PATCH

### A (recommended): throw-window handoff to the local AI, plus a one-test diagnostic

**Step 1, diagnostic** (one session, decides between A and B). Hook `0xBE3810(atk, def, kind)` and `0xBE3A90(atk, def, kind, paramPair)`.
- Plain `__fastcall`, return `bool`. Prologues:
  - `48 89 5C 24 18 55 56 57 41 54 41 55 41 56 41 57`
  - `48 89 5C 24 08 48 89 6C 24 10 48 89 74 24 18 57`
  - No RIP-relative instruction in the first 16 B. A 14-byte jump ends on an instruction boundary in both.
- Log when `atk` is the local player and `def` is engaged NetAI. Rate-limit to 1/s per enemy:
  ```
  throwdiag: enemy <id> netai <0/1> e3810 <r> e3a90 <r> tm7A <b> tm7B <b> mf70 <hex>
             s170 <0/1> dce60 <i> hp <n> sp [<ids>] node <0/1>
  ```
- Compare the same enemy with handoff on and off. A false `e3810` with `tm7A=tm7B=0` points to throwable state (HKS/TAE). A false `e3a90` with a missing SpEffect points to candidate 1. Hooking `0xBDF930` (prologue `4C 8B DC 57 48 83 EC 50 …`) gives the finder-level yes/no.

**Step 2, fix.**
- **When to hand off:** a NetAI-engaged enemy goes back to the local AI (`slot37` stops answering `0xA14B50(chr,0)`; offline the game then picks ComManipulator next frame [VAR, `0xA4DB20`]) while at least one of these holds:
  1. the local player is within ~5 m and in the rear 120° of the enemy (the stealth window);
  2. local posture is at the break threshold, or the enemy's local animation is the posture-broken one;
  3. the def throw is active (`node` test above).
- **While handed off:** stop writing the NetAI pending bits for it; keep HP authority unchanged.
- **When to re-engage:** the throw is inactive for ≥ 1 s, the player is more than 6.5 m away (hysteresis), and the enemy is alive.
- **Why it works:** the local AI, HKS, TAE and SpEffects then produce exactly the single-player gate state relative to the client player. This holds whichever candidate (1-3) is the real one.
- **Risks:**
  - the local AI may attack the client while handed off (a short "two AIs" desync);
  - the re-engage snaps the enemy to the host pose (NetAI snap rules);
  - ChrCtrl rebind (`0x7DE3D0`) happens mid-anim; the window is chosen so that it is not mid-throw.
- **Log lines:**
  - `throw: handoff <id> reason behind|posture|node dist <m>`
  - `throw: node active <id> frames <n>`
  - `throw: re-engaged <id> after <ms>`
  - existing `NetAI active (+0x58 == +0x2008)` counts should dip only for those ids.

### B (cheaper if the diagnostic shows candidate 1): mirror the deciding state from the host

- Host, per enemy at the movement tick: send the ids of SpEffects whose id matches any ThrowParam `+0x70` (or the HKS-written `TM+0x7B` byte).
- Client: apply the SpEffect locally with `0x9E9C00` and remove it with `0xBFB6D0(list, id)` (`0xC00340` only keeps a node for one more update, IMPLEMENTATION-NOTES §15 review fixes), with the arguments the game itself uses in `0xBE2D60`.
- **Unverified [ARV]:** the full argument list of `0x9E9C00` was not decoded; copy it from `0xBE2D60`'s call at `0xBE321E`. Writing `TM+0x7B` directly races with HKS act 161, which rewrites it every behavior update. Do it only right after the behavior update, or prefer the SpEffect route.
- **Log line:** `throwsync: enemy <id> sp +<id>/-<id>`, plus the `throwdiag` line now showing `e3a90 1`.

### C (always, small): freeze NetAI during an active throw

- While `node != 0 || node+0x144 > 0` on a NetAI enemy:
  - do not set the movement pending bit `0x01` (behavior is already gated by the game);
  - zero `manip+0x370` / `+0x380` (velocity and angular velocity) and `manip+0x390/+0x391` (snap flags) each frame before the chr update;
  - or simply release it as in A.
- **Unverified [ARV]:** that `0x7EA5C0` applies the stored velocity every frame even without a new record (its else-branch calls slot 41 with `velocity * dt`). Zeroing it stops the drift.
- **Log line:** `throw: netai frozen <id> frames <n>`. Verify by eye: the deathblow animation completes and the enemy is not yanked or snapped.

## Open items (verify live)

1. Run the step-1 diagnostic against a stealth target and a posture-broken target. It shows which of candidates 1-3 is real.
2. Check whether `0x7DCE60` and slot 170 return the same values for an AI-driven and a NetAI-driven copy of the same enemy (expected: same).
3. The ThrowParam layout beyond `[0]`, `[1]`, `+0x30/+0x32` (ray lengths ×0.01), `+0x3B` bit 2 (skip ray), `+0x60` (kind), `+0x6C/+0x70` (SpEffects) is read from code only. The field names are guesses.
