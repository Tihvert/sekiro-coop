# RE findings 2: flags, rest, camera, boss state, animation (Sekiro 1.06 / 1.6.0.0)

Source: IDA 9.3 headless on the decrypted in-memory dump `F:\SekiroRE\quick.i64` (image base `0x140000000`). Static analysis only: the game was not run or modified.
All addresses are **RVAs** (add the module base at runtime). Prologue bytes were read from the dump, so they are the real runtime bytes.

Scripts: `re/re2_flags.py`, `re/re2_cam.py`, `re/re2_misc.py`, `re/re2_boss.py`, `re/re2_boss2.py`, `re/re2_idol.py`, `re/re2_sync.py`.
Decompiled output: `F:\SekiroRE\outRE2\{flags,cam,misc,boss,boss2,idol,sync}\` (one `.c` per function, plus `summary.txt`).

Confidence tags:
- **[V]** verified by decompile
- **[I]** inferred from decompile and context
- **[G]** guess or outside knowledge, not checked here

Every [I] and [G] item needs a live check before anything depends on it.

## Globals used below

| Global | RVA | Notes |
|---|---|---|
| SprjEventFlagMan* | `0x3D55FE8` | [V] Matches SekiroTool. RTTI `NS_SPRJ::SprjEventFlagMan` vtable at `0x296E100`. |
| DebugEventMan | `0x3D55A08` | [V] Used only by the debug flag-watch path in SetEvent. |
| WorldChrMan | `0x3D7A1E0` | Player ChrIns at `+0x88` [V]: every Rest caller uses it. |
| FieldArea | `0x3D5C0A0` | `+0x20` GameRend*, `+0x28` block index, `+0x30` ChrCam* |
| GameMan (probable) | `0x3D5C0D0` | [I] Warp/restart state (see §2). |
| GameDataMan | `0x3D5AAC0` | `+0xC0` = last boss entity id (§4) |
| MenuMan | `0x3D67408` | [V] Matches SekiroTool. Boss gauge slots (§4). |
| SprjCameraImp singleton (probable) | `0x3D85008` | [I] The final camera is pushed here each frame. |
| Render width / height | `0x3B1D100` / `0x3B1D104` (int32) | [I] Used to compute the camera aspect. |

---

## 1. Event flags

### SetEventFlag: RVA `0x6C4520` [V]

```cpp
// SprjEventFlagMan::SetEventFlag (88 direct callers)
void __fastcall SetEventFlag(SprjEventFlagMan* self, uint32_t flagId, bool on, bool a4 /*pass 0*/);
```

- `self = *(void**)(base + 0x3D55FE8)`. The call returns immediately if `*(u8*)(self+0x228) == 0`, which means the flags are not loaded yet.
- Body: `blk = Resolve(self, id, a4)` (`0x6C63F0`), `bit = id % 1000`, then `dword[bit>>5]` gets mask `1 << (31 - (bit & 31))` set or cleared.
  - Bits are stored MSB-first within each dword.
  - `blk` points to a 16-byte record. Its first qword is the bit array: 1000 bits per record.
- `a4` is used only by the debug flag-break/watch feature (`self+0x2A8` enabled → DebugEventMan notify). SekiroTool passes `r9 = 0`.
- How the flag id splits into tables (from the resolver `0x6C63F0`):
  - `id/10^7 % 10` is the table group. It indexes `*(self+0x218)`, stride 24.
  - `(id/10^5 % 100, id/10^4 % 10)` = (area, block) is mapped through FieldArea's world-block list (`0x82B210`) to a block index. Area 0 / block 0 is the global block.
  - `id/1000 % 10` selects the 16-byte sub-record (record address = `table + 168*blockIdx + 16*digit`).
- `self+0x220` selects one of two flag banks: bank 0 at `self+0x28`, bank 1 at `self+0x120` (stride 0xF8). `self+0x218` is the active bank pointer.
  - `0x6C48C0` and `0x6C4920` temporarily switch the bank and call SetEventFlag.
  - [I] Bank 1 is probably the "Reflection / Remnant" boss-replay mode, because `0xA90030` checks `self+0x220 == 1` and in that case forces a full heal.

Prologue:
```
+00 40 55                 push rbp
+02 41 54                 push r12
+04 41 55                 push r13
+06 41 56                 push r14
+08 48 83 EC 58           sub  rsp,58h
+0C 80 B9 28 02 00 00 00  cmp  byte [rcx+228h],0
+13 45 0F B6 E1           movzx r12d,r9b
+17 45 0F B6 E8           movzx r13d,r8b
```
- 5-byte jmp: relocate 6 bytes (`+00..+05`) and resume at `+06`.
- 14-byte abs jmp: relocate 19 bytes (`+00..+12`) and resume at `+13`. The flags from `cmp` survive the jump back.
- There is **no RIP-relative instruction** in the first 19 bytes.

### GetEventFlag: RVA `0x6C3E60` [V]

```cpp
bool __fastcall GetEventFlag(SprjEventFlagMan* self, uint32_t flagId, bool a3 /*pass 0*/); // 127 callers
```
- It is a pure read: resolve, then test the bit. Returns 0 if the flags are not loaded or the block is unknown.
- `0x6AA860` is a wrapper that loads the singleton itself: `bool f(void* unused, int id)`. It has 36 callers (the ESD/EMEVD-side "GetEventFlag").
- Prologue:
  ```
  40 53 | 48 83 EC 20 | 80 B9 28 02 00 00 00 | 8B DA | 74 4D (jz rel8 at +0F) | E8 .. (call rel32 at +11)
  ```
  - 5-byte jmp: relocate 6 bytes.
  - 14-byte jmp: relocate 15 bytes (stop before the `jz` at `+0F`).
  - No RIP-relative instruction in the first 15 bytes. It is hot (127 callers, called every frame), so don't hook it. Just call it.

### Other flag entry points [V]

| RVA | What | Goes through SetEventFlag? |
|---|---|---|
| `0x6AAAB0` | `char f(void* unused, uint id, bool on)`: singleton wrapper, 32 callers (ESD/EMEVD SetEventFlag). Prologue `48 89 5C 24 08 / 57 / 48 83 EC 20 / 48 8B 0D <rel32>` has a **RIP-relative** load at `+0A`. | yes |
| `0x6C4890` | `SetEventFlagAndSync`: SetEventFlag, then `0x8D9ED0`, which sends **net message type 26 {flag, on}** if `IsFlagNetSynced(flag)`. Leftover DS3 co-op flag sync. | yes |
| `0x6C48C0` / `0x6C4920` | Set the flag in bank 0 or bank 1 explicitly. | yes |
| `0x6C47A0` | `SetEventFlagValue(self, id, int bitCount, uint value)`: writes a multi-bit field **directly**. | **no** |
| `0x6C3ED0` | `GetEventFlagValue(self, id, bitCount, a4)`: multi-bit read. | n/a |
| `0x6C55D0` | vtable method (0 direct xrefs): ORs a bit in directly on one path (and calls SetEventFlag on another). | partly |

**Choke point:** hook `0x6C4520` (SetEventFlag). It sees all single-bit writes, including every wrapper above.
- For full coverage, also hook `0x6C47A0` (multi-bit values, rare).
- Save-game load restores flags in bulk without any of these. Snapshot the flags after load instead.
- In the hook, call the original GetEventFlag before forwarding, to tell real changes from rewrites of the same value.
- Prologue of `0x6C47A0`:
  ```
  48 89 5C 24 10 | 48 89 6C 24 18 | 56 | 48 83 EC 20 | 80 B9 28 02 00 00 00
  ```
  - 5-byte jmp: relocate 5 bytes.
  - 14-byte jmp: relocate 15 bytes.
  - No RIP-relative instruction.

### Calling from our DLL

```cpp
auto fm = *(void**)(base + 0x3D55FE8);
if (fm && *(uint8_t*)((char*)fm + 0x228)) {
    ((void(__fastcall*)(void*, uint32_t, bool, bool))(base + 0x6C4520))(fm, id, on, false);
    bool v = ((bool(__fastcall*)(void*, uint32_t, bool))(base + 0x6C3E60))(fm, id, false);
}
```
- Call from the game thread, for example from our existing UpdateCoords hook. SetEventFlag can also trigger debug/event side paths. SekiroTool calls both from a foreign thread (`CreateRemoteThread` shellcode) without problems, so a read from another thread is low-risk.
- A map flag whose (area, block) does not resolve is silently ignored. That is the `Resolve` failure path.

### Which flags are "world" and which are "per player"

**[V] The game has its own rule:** `IsFlagNetSynced` at RVA `0x6C4240`. It decides which flags are mirrored to co-op partners. Let `top = id/10^7 % 10` and `k = id/1000 % 10`:

| top digit | synced (world/shared) when | not synced (local) |
|---|---|---|
| 0 (ids < 10,000,000) | `k ∈ {0,1,5,9}` | `k ∈ {2,3,4,6,7,8}` |
| 1, 2 (map flags `1AABBkxxx`, `2…`) | `k ∈ {0,1,4,5,6,7}` | `k ∈ {2,3,8,9}` |
| 6 | `k == 0` | otherwise |
| 3, 4, 5, 7, 8, 9 | never | always |

[I] This rule comes from DS3's co-op code. It is a good first split:
- "synced" = world progress: bosses, doors, shortcuts, idols.
- "not synced" = per-player: items, NPC quest state (the `7xxxxxxx` NPC flags are never synced).

Known ids:

| Flag id(s) | Meaning | Source |
|---|---|---|
| `1AABB000n` (e.g. 11100000–11100007, 12500000–…) | Idol unlocked | [V] SekiroTool `IdolEventFlags` list (sets them to unlock idols). Under the rule above: synced. |
| 11100850 (snake escaped), 12505888/9 (dragon), 11115900 (Emma skip) | Map/boss progress | SekiroTool `GameEvent.cs`. Synced. |
| 71100500, 71110020 | NPC/boss state | SekiroTool. `7xxxxxxx` = NPC flags, never synced. |
| 6911 (Kuro's charm), 9830 (demon bell), 9800/9801/9802 (time of day), 8301–8306 (story states) | Global | SekiroTool. 6911 and 8xxx are local, 98xx is synced. |
| `AABB08xx`-style "boss defeated" | [G] DS3 convention (`13000800` = Vordt). Not checked for Sekiro. | — |
| Item pickup / treasure | [G] Not determined. The reliable way is runtime data: every `ItemLotParam.getItemFlagId` (and `ShopLineupParam.eventFlag`) is an item flag. Read those params at runtime and classify by membership instead of by range. | — |

---

## 2. Rest at idol

### "Rest" (SekiroTool Functions.Rest): RVA `0xA2BE40` [V]

```cpp
bool __fastcall ChrIns_RestReplenish(ChrIns* player, uint32_t amount); // 4 callers
```
- It **only replenishes the player**:
  - item/gourd refills via `0x79EDC0` on `vfunc[0x1B8](player)+0x278` ([I] the 0x1B8 vfunc probably returns PlayerGameData)
  - `0xA35F60` (two more refill slots)
  - `0xA36070` (clears a SpEffect category, `0x40000000`)
  - `0xA344A0`
  - `0x9E99A0(player, 100..109, 0)` in a loop ([I] resets status / SpEffect slots 100–109)
- It does **not** respawn enemies, does not need an idol, and does not open a menu.
- If `EventFlagMan+0x220 == 1`, `amount` is forced to 100.
- SekiroTool calls it with `(PlayerIns, 10)`. The game passes 0 (debug), 100 (after load), or a per-idol value.
- Safe to call remotely: rcx = `*(WorldChrMan+0x88)`. Prefer the game thread.

Prologue:
```
48 89 5C 24 08 | 48 89 6C 24 10 | 48 89 74 24 18 | 48 89 7C 24 20 | 41 56 | 48 83 EC 20
```
- 5-byte jmp: relocate 5 bytes.
- 14-byte jmp: relocate 15 bytes.
- No RIP-relative instruction.

Callers [V]:

| Caller | When it calls Rest |
|---|---|
| `0x665B50` | Idol object per-frame vfunc. If `0x76E7C0()` (= pending rest-idol id) equals this idol's entity id (`this+0x20`), it calls `Rest(player, idolValue)`. **This is the real "rested at idol" path.** |
| `0xA73430` | Player update. If `GameMan+0xB47` ("replenish on next load") is set, it calls `Rest(player,100)` and clears the flag (`0x832C90(0)`). This runs after warp, travel or respawn. |
| `0x11C9370` | Another path, gated on `*(int*)(player+0x70) ∈ {0,1,8}` (read via `0x9EEEC0`). It then calls `0xA4C0F0` and `0x899500`. [I] An item or event-driven replenish. |
| `0x67B370` | Debug, `Rest(player, 0)`. |

### Idol request mailbox [V]

Fields at RVA `0x3AFEB88..0x3AFEBA4`:
- Writers (ESD/talk-command style, signature `(void*, int idolEntityId)`):
  - `0x68E5B0` sets `0x3AFEB90` and `0x3AFEB98`
  - `0x68E5D0` sets `0x3AFEB98`
  - `0x68E5E0` sets `0x3AFEBA0`
- Once per frame, `0x76EFF0` latches the next fields: B90→B94, B98→**B9C**, BA0→BA4. It then resets the write fields to -1.
- Readers: `0x76E7B0`, `0x76E7C0` (returns **`0x3AFEB9C`**), `0x76E7D0`.
  - The idol object (`0x665B50`) compares them with its own entity id. A match on B9C triggers Rest plus the idol SFX.
- SekiroTool's `RequestRespawn` experiment writes `0x3AFEBA0 = 1101950` directly, which is this mailbox.

### Detecting that the local player rested

- **Best:** poll `*(int*)(base+0x3AFEB9C)` once per frame. It equals the idol entity id (e.g. 1101950) during the frame the rest is processed, and -1 otherwise. [I] Writer B98 is the "rest" request.
- **Alternative:** hook `0xA2BE40` with `rcx == player`. Then check the return address or `GameMan+0xB47` to exclude the warp/load replenish. This fires for every replenish.

### Triggering a remote rest

- **Light (player replenish only):** call `0xA2BE40(player, 100)`.
- **Idol-style:** write the idol entity id to `0x3AFEB98` on the game thread, before the latch runs. [I] The idol object with that id then runs Rest and its SFX next frame. It only works while that idol object is loaded (same map).
- **Full rest with enemy respawn:** not found as a single function. [I] It goes through the ESD rest sequence plus a warp/reload.
  - The closest callable is Warp, RVA `0x68E870` [V]: `__int64 f(void* unused, int idolEntityId, int idolParamIdx /*-1 ok*/)`.
    1. Sets restart/warp target ids in GameMan (`+0x1C = id-980`, `+0x20`, `+0xB50 = id+1000`).
    2. Then `0x6D7070` requests the map reload.
  - SekiroTool's asm template passes only `rdx = idolId` and `r8d = 0`.
  - Warp prologue:
    ```
    40 57 | 48 83 EC 40 | 48 C7 44 24 20 FE FF FF FF | 48 89 5C 24 50
    ```
    5-byte jmp relocates 6 bytes, 14-byte jmp relocates 15 bytes. No RIP-relative instruction.
  - Caller `0x6795F0` shows the full "travel to idol" order: `0x14068CDD0`/`0x14068D040`, then `GameDataMan+0x7B = 1`, then Warp, then `0x832CB0(id+1000)`, `0x833030`, `0x832960(1)`, `0x832EC0(1)`.

---

## 3. Camera and world-to-screen

### Pointer chains

```
FieldArea  = *(void**)(base + 0x3D5C0A0)
GameRend   = *(void**)(FieldArea + 0x20)        // RTTI NS_SPRJ::GameRend (ctor 0x839CC0)
RenderCam  = *(SprjPersCam**)(GameRend + 0x18)  // [I] main camera handed to the renderer
ChrCam     = *(ChrCam**)(FieldArea + 0x30)      // [V] ChrCam derives from SprjPersCam (ctor 0x733850 calls 0xF00C30)
DebugCam   = *(SprjPersCam**)(GameRend + 0xE8)  // free cam; GameRend+0xE0 = free-cam mode (SekiroTool)
```

- `GameRend+0x10` is a `GameRendCameraSet` (vtable `0x29DF200`). Its constructor `0x83AC90` allocates two SprjPersCams at `GameRend+0x18` and `+0x20`.
- `GameRend+0x28` holds a third one.
- The per-frame `0x83B150`:
  1. blends `+0x18`
  2. copies `+0x18` to `+0x20`
  3. pushes `+0x18` into the SprjCamera singleton (`0x3D85008`, `0xB227F0`)
  4. if free cam is on (`+0xE0 != 0`), overwrites `+0x18` from DebugCam

  So **`GameRend+0x18` is the camera that is actually rendered**. [I] Confirm live: its position should sit near and behind the player.

### SprjCam / SprjPersCam layout [V]

From ctors `0xF00030` and `0xF00C30`, getter `0x734240`, and `0xF01260`:

| Offset | Type | Content |
|---|---|---|
| +0x00 | vtable | SprjPersCam `0x29DF2A8` |
| +0x10 | float4 | row 0 = right axis (X) |
| +0x20 | float4 | row 1 = up axis (Y) |
| +0x30 | float4 | row 2 = forward / look direction (Z). The ctor stores the normalized look vector here. |
| +0x40 | float4 | row 3 = **camera world position** (w = 1). Matches SekiroTool `DebugCamCoords = GameRend+0xE8 → +0x40`. |
| +0x50 | float | vertical FOV in **radians** (default 0.7505 rad ≈ 43°) |
| +0x54 | float | aspect (default 16/9; set to `width/height`) |
| +0x58 | float | near plane (0.08) |
| +0x5C | float | far plane (10000) |

- This is the camera's **world transform** (camera to world), not a view matrix.
- It is row-major with row-vector convention (D3D style): the translation is in the 4th row.
- `0xF01260` (SprjPersCam vfunc 3, builds render parameters):
  - sets `aspect = W/H` from the globals `0x3B1D100` / `0x3B1D104`
  - if `aspect < 16/9`, widens the vertical FOV (keeps the 16:9 horizontal FOV)
  - outputs fov, near, far, W, H and aspect

### Projecting a world point to pixels

[I] The math is standard. Handedness and the sign of X are not checked live, so flip X if overlays come out mirrored.

```cpp
struct V4 { float x, y, z, w; };
const V4* m = (V4*)((char*)cam + 0x10);   // m[0]=right m[1]=up m[2]=fwd m[3]=pos
float fov = *(float*)((char*)cam + 0x50);
int W = *(int*)(base + 0x3B1D100), H = *(int*)(base + 0x3B1D104);   // or swapchain backbuffer size
float aspect = (float)W / H;
float vfov = fov;
if (aspect < 16.f/9.f) vfov = 2*atanf(tanf(fov/2) * (16.f/9.f) / aspect);   // mirrors 0xF01260
V4 d = { p.x - m[3].x, p.y - m[3].y, p.z - m[3].z };
float cx = d.x*m[0].x + d.y*m[0].y + d.z*m[0].z;
float cy = d.x*m[1].x + d.y*m[1].y + d.z*m[1].z;
float cz = d.x*m[2].x + d.y*m[2].y + d.z*m[2].z;          // depth; <= near -> behind camera
if (cz <= 0.08f) return false;
float t = tanf(vfov * 0.5f);
float ndcX = cx / (cz * t * aspect), ndcY = cy / (cz * t);
sx = (ndcX * 0.5f + 0.5f) * W;  sy = (1.f - (ndcY * 0.5f + 0.5f)) * H;
```

- Read the camera inside our Present/overlay hook. These are plain reads and need no hook.
- For pixel-exact results, prefer the swapchain backbuffer size over the globals (render scale can differ).

---

## 4. Boss fight state

### Boss HP gauge (menu): the reliable "in boss fight" signal [V]

- The EMEVD/ESD command `SetBossGauge` (binding registered at `0x93490`, handler `0x68D2C0`) calls `SetBossGaugeImpl` at RVA `0x67C7C0`:
  ```cpp
  f(void* evMgr /*qword 0x3D55600*/, int bossEntityId, uint slot /*0..2*/, int nameTextId, int, int, bool alt=0)
  ```
- It resolves the ChrIns with `0x6D5B20` (GetChrInsByEntityId wrapper of `0x6C1A10`) and writes the gauge slot:
  - slot = `MenuMan + 0x2E10 + 0xA8*slot` (3 slots; the `alt=1` variant uses `MenuMan+0x2C08`)
  - `slot+0x00` int = name text id (FMG)
  - `slot+0x04` int = `*(int*)(ChrIns+0x08)`, the boss ChrIns handle id ([I] FieldInsHandle)
  - `MenuMan + 0x3008 + 4*slot` is zeroed (anim/timer)
- `ClearBossGauge` (`0x688E30` → `0x8C3A20`) resets all 3 slots: the qword at `slot+0` becomes `-1`.
- **Detection:** `MenuMan = *(base+0x3D67408)`. You are in a boss fight if any slot `s ∈ 0..2` has `*(int*)(MenuMan+0x2E10+0xA8*s+4) != -1`.
- Prologue of `0x67C7C0`:
  ```
  48 89 5C 24 10 | 48 89 74 24 18 | 48 89 7C 24 20 | 41 56 | 48 83 EC 20 | 80 7C 24 60 00
  ```
  5-byte jmp relocates 5 bytes, 14-byte jmp relocates 15 bytes. No RIP-relative instruction. Hooking it gives the boss entity id directly in `edx`.
- Prologue of `0x8C3A20`: `48 81 EC 98 00 00 00 | 48 8B 05 <rel32>`. The **RIP-relative** load of MenuMan is at `+07`. A 5-byte jmp is fine (relocate 7 bytes). A 14-byte jmp must fix that displacement.

### Boss ChrIns

- `SetBossGaugeImpl` also calls `0x7B4600`, which writes **`GameDataMan+0xC0 = bossEntityId`** (unless a net-state check `0x8E44C0` blocks it). [V] for the write. [I] that it is not cleared afterwards.
- Get the ChrIns with the known `GetChrInsByEntityId` (`0x6C1A10`, pointer argument), or scan the characters for `*(int*)(ChrIns+8) == slot+4`.
- `ChrData+0x25C` "CurrentBossNode" (SekiroTool) is the boss phase/life node that SekiroTool writes to skip phases. [I] It shows phase, not whether a fight is running.
- Other related bindings found as strings (not followed): `ReportBossArea` (`0x69CD80`), `ReportClearedBossArea`, `Failed_BossAreaMission[_LeaveMap]`, `SetCamModeParamTargetIdForBossLock` (writes `GameMan+0x2C`, handler `0x68D3B0`).

---

## 5. Current animation id (TimeAct module) [V layout, I semantics]

- `SprjChrTimeActModule` (vtable `0x2A7B4D0`, ctor `0xB5BD20`).
- Its debug-menu printer `0xB5C9B0` prints `"TimeAct ID:%03d_%06d, srt:%.2f, end:%.2f, duration:%.2f, start:%u"` from a **ring buffer of the last 10 TimeActs**.

| Offset | Content |
|---|---|
| +0x20 + 0x14*i | entry i: `+0x0` int TimeAct/anim id (full id, e.g. `aXXX_YYYYYY` → `id/1e6`, `id%1e6`); `+0x4` float start; `+0x8` float end; `+0xC` float duration; `+0x10` uint start frame |
| +0xE8 | int write index (head) |
| +0xEC | int read index (tail, oldest) |
| +0xF4 | int, set by `0xB5C910` (`mov [rcx+0F4h],edx; ret`), reset to -1. Also printed in mode 1 with `+0xFC` (float) and `+0x100` (int, last queried id) |

- [I] Current animation = `entry[(head + 9) % 10].id` when `head != tail`. The module pointer is `*(ChrIns+0x1FF8 → +0x10)`; that table slot came from your notes and was not re-checked here.
- Alternatives that need no reading: SekiroTool `ForceAnimation` (`0x6D5A90`, a 5-byte thunk to `0x753860`) and `ForceAnimationByChrEventModule` (`0xBD84C0`), for setting animations.
- `IsExistAnimID` (Lua binding `0x5D4E80`) only checks whether an animation exists.

---

## Hook summary

| Function | RVA | Bytes to relocate (5 / 14-byte jmp) | RIP-relative in range? |
|---|---|---|---|
| SetEventFlag | 0x6C4520 | 6 / 19 | no |
| SetEventFlagValue | 0x6C47A0 | 5 / 15 | no |
| GetEventFlag | 0x6C3E60 | 6 / 15 | no (don't hook; hot) |
| SetEventFlag wrapper | 0x6AAAB0 | 5 / — | **yes, at +0A** |
| Rest (replenish) | 0xA2BE40 | 5 / 15 | no |
| Warp to idol | 0x68E870 | 6 / 15 | no |
| SetBossGaugeImpl | 0x67C7C0 | 5 / 15 | no |
| ClearBossGauge | 0x8C3A20 | 7 / — | **yes, at +07** |
| GameRend camera update | 0x83B150 | 5 / 16 (`mov rax,rsp` first; jmp, never call, into the trampoline) | no |

## Open items (verify live)

1. Check that `GameRend+0x18` +0x40 equals the camera position, and the sign of X in the projection.
2. Check that `0x3AFEB9C` shows the idol id for one frame when resting.
3. Check that the boss gauge slot `+4` becomes non -1 at a boss (e.g. Gyoubu) and returns to -1 on death or win.
4. Check that the TimeAct ring buffer's newest entry tracks the player's animation.
5. Find item-flag ranges via ItemLotParam at runtime.

---

## § Ghost animation playback (2026-10-22)

Scripts: `re/ghostanim.py` … `re/ghostanim5.py` (headless `idat -A -S<script> -L<log> F:\SekiroRE\quick.i64`, same dump as above). Output: `F:\SekiroRE\out16` … `out20`. Static analysis only; nothing below has been run in game yet. RVAs are relative to the module base.

### Why the current path returns null [V]

- `chr->vtbl[247]` (`0xA29520`, same slot in PlayerIns and ReplayGhostIns) just returns the qword at **`ChrIns+0x2118`**. That is not the event module. It is an `EventChrEntity` adapter (vtable `0x29AC0B8`, 24 bytes: `+0x08` entity id, `+0x10` back-pointer to the ChrIns).
- The adapter is created lazily by `0xA2BD90(chr)`, which also registers it with the event entity manager (`qword 0x3D5A110`) under the chr's entity id (`vtbl[163]`). The ChrIns ctor zeroes `+0x2118`. Our plain ghost is never registered, so the field stays 0 and the log line `event module null` is expected.
- `EventChrEntity` slot 7 (`+0x38`) is `0x750E00` ("force": unconditional) and slot 6 (`+0x30`) is `0x751120` ("play": only if the event module is idle). Both do nothing but forward to the **real event module at `modules + 0x58`** (`*(ChrIns+0x1FF8) + 0x58`). So the adapter can be skipped entirely.

### Module table indices [V]

The module table is filled by `0xA002F0` through `0xC06D70(table, index, module)`. Index × 8 = offset. Relevant: index 5 → `+0x28` SprjChrBehaviorModule (created by `0xBD8C10`), index 11 → **`+0x58` SprjChrEventModule** (`0xBD9070`, ctor `0xBD8050`), `+0x10` TimeAct, `+0x30` BehaviorSync, `+0x88` (the module the event path also checks; see below). The calls are unconditional, so every ChrIns has an event module. [I, high] it is non-null on our ghost: the ghost's per-frame update dereferences it every frame (below) without crashing.

### Path A: force an animation through the event module (simple)

`ForceAnimationByChrEventModule`, RVA **`0xBD84C0`** [V]:

```
void f(SprjChrEventModule* evm /*rcx*/, int animId /*edx*/)
```
- Stores `animId` at `evm+0x10` (ctor value -1 = none) and also at `*(ChrIns+0x20) + 0x24`, where ChrIns = `*(evm+0x08)` (`0xB271D0` returns that owner and only asserts if it is null).
- Prologue `40 53 | 48 83 EC 20 | 8B DA | 89 51 10 | E8 …`: there is a call at `+0x0B`, so don't hook it, just call it.

`IsBusy` `0xBD8430(evm) -> bool` [V]: true if a request is already pending (`evm+0x10 != -1`) or the owner's `modules[0]+0x10` has bit 1 set. This is what the "play, not force" variant checks first.

Consumption [V]: the event module's per-frame apply `0xBD81B0(evm)` runs from the ChrIns update `0xA04850` (`0xA04850` is called by PlayerIns update `0xA37560`, vtbl slot 22). ReplayGhostIns slot 22 (`0xA39D40`) calls `0xA37560` first and then its own `0xA39C10`, so **the apply runs for our ghost every frame**. There is no early return before the call. The apply:
1. resets `*(chr+0x20)+0x28 = -1`;
2. if a request is pending and all three gates pass: (a) `*(*(chr+0x18)+0x30)+0x8C` bit 0 clear (`0x9E5C00`); (b) `modules[0]+0x10` bit `0x8000` clear; (c) `0xBE0930(modules+0x88)` false. If so, it takes the hkb graph of the behavior module (`0xB41C40`) and calls `0xFF1DA0(graph, animId)`;
3. `0xFF1DA0` formats the hkb event name **`L"W_Event%04d"`** (prefix table `0x3B151F0`: `[0]="W_Event"`, `[1]="W_Throw"`), maps the name to an event index for that graph (`0xFF0BD0`) and raises it (`0xFF1D10`). On success it writes `*(chr+0x20)+0x28 = animId` and sets bit `0x20` in `modules[0]+0x54`;
4. clears the request (`evm+0x10 = -1`, `*(chr+0x20)+0x24 = -1`), whether or not it played.

What this means [V for the mechanism, I for the content]: Path A plays an animation **only if the c0000 behavior graph contains an event named `W_Event<id>`**. Which ids have one is data in `c0000.behbnd` and was not checked here. It works for EMEVD/ESD-style event animations. Ordinary combat and locomotion animations are normally reached through other hkb events and variables, so many ring ids will silently do nothing (the request is dropped after one frame, with no error).

Thread: game thread, before the ghost's chr update in the same frame. The UpdateCoords hook qualifies (it runs inside the physics update, so the request is consumed next frame at the latest). Confidence [I].

### Path B: mirror hkb events (what replays do)

ReplayManipulator apply-frame `0x7F4C90` (re/out7) does **not** use an animation id at all [V]. For each frame it calls the ghost's **BehaviorSync module (`modules + 0x30`)**:
- `0xB4EFE0(bsync, frame)`: applies hkb variable blocks (`0xFFEBB0(*(bsync+0x10), frame+0x210)`, five blocks at `+0x58/+0x670/+0x988/+0xFA0/+0x1BB8`) and then `0xB4E9C0`.
- `0xB4E9C0` turns the frame's list of u16 sync-event ids into names (`0xFE9240`), then into graph indices (`0xFF0DF0`), and raises each with `0xFF1D10`.

The low-level hkb API, all [V]:

| RVA | Signature | Notes |
|---|---|---|
| `0xFF1D10` | `bool raise(HkPair* p, int eventIdx)` | `p = {graph, aux}`, only `p->graph` is read. Returns 0 if graph is null or idx < 0. Pushes `{idx, 0,0}` (24 B) onto the queue at `graph+0xD0` (grows it with `0xFF20D0`). Only queues the event; the graph consumes it in the behavior update (`0xB44630`, called from `0xA04850`). |
| `0xFF1CD0` | `bool raise_by_name(HkPair* p, const wchar_t* name)` | Name to index (`0xFF0BD0`), then `raise`. |
| `0x1001270` | `bool raise_on(HkChr* x, int eventIdx)` | Builds `p = {*(x+0x28), x+0x38}` itself; `x = *(behaviorModule+0x10)`. |
| `0xB41C40` | `HkPair* get_pair(SprjChrBehaviorModule* bm, HkPair* out)` | `out = {*(x+0x28), x+0x38}` with `x = *(bm+0x10)`; zero if `x` is null or the global `0x3D93050` is null. |

Prologue of `0xFF1D10`: `40 53 | 48 83 EC 40 | 48 8B 19 | 48 85 DB | 74 72 | 85 D2 | 78 6E`. The 5-byte jmp relocates 6 bytes (`+00..+05`), and there is no RIP-relative instruction. A 14-byte jmp would cut into `jz rel8` at `+0C`, so don't use it. 16 direct callers, among them ChrCtrl/ActionRequest code at `0xB44xxx`/`0xB46xxx`/`0xBDF310`, the event module, and BehaviorSync. Sender idea: hook it, keep only calls whose `p->graph == player graph`, and send `eventIdx`. The receiver calls `0x1001270(ghost_x, idx)`.

Caveats: [I] event indices should match between two c0000 graphs (same behavior file), but the replay code deliberately converts through names, so check it live (`raise` returns true and the ghost visibly reacts). Internal hkb transitions and **hkb variables** (locomotion speed/direction) are not covered by events, so a ghost driven only by events will attack/roll/dodge but will not blend walk and run. Copying variables is the replay's `0xFFEBB0` route. Its frame layout (~10 KB, packed by the sender-side capture `0xB4F1A0`) is not decoded here.

### What the TimeAct ring holds [V layout, I semantics]

- The writer is `0xB5C580(taModule, int taeId, float start, float end, int)`. It looks the id up in the module's TAE data (`*(ta+0x10)`, `0x98ED70`), runs the TAE events (`0xB58420`/`0x654B70`), and stores `{id, start, end, …}` at `ta+0x20+0x14*head`, then `head = (head+1)%10` with **head at `+0xE8`**. So `(head+9)%10` is the newest entry [V], as anim.rs assumes.
- Its only caller is thunk `0xB5C920(taeId, …, ChrIns*)`, which is reached via a function pointer, so presumably from hkb clip playback. The id is the **full TAE id `category*1_000_000 + anim`** (the debug printer prints `%03d_%06d`, i.e. `aCCC_AAAAAA`).
- Path A's `W_Event%04d` expects the bare animation number, e.g. 3000 / 7000 / 20000. [I] For category 0 that is `id % 1_000_000`. The behavior's `AnimIdOffset` adapter (`SprjHkBehAnimIdOffsetAdapter`) can shift categories, so for `id / 1_000_000 != 0` the right event number is unknown.

### Confidence summary

| Claim | Level |
|---|---|
| `vtbl[247]` returns `ChrIns+0x2118` EventChrEntity, null until registered | V |
| Event module = `modules+0x58`; `0xBD84C0(evm, anim)` is the force call | V |
| Event apply runs every frame for ReplayGhostIns (slot 22 chains to PlayerIns update) | V (static) |
| Path A only plays ids that have a `W_Event<id>` hkb event | V mechanism / which ids: unknown |
| Ghost's event module and behavior graph exist and are live | I (high) |
| Replays animate ghosts by raising hkb events plus variable blocks, not by anim id | V |
| Event indices match between player and ghost graphs | I (medium) |
| Ring id = `cat*1e6 + anim`; send `id % 1e6` for Path A | I |

### Path A′: raise `W_Event<id>` on the behavior graph directly (cross-check with docs/WEB-ANIM-RESEARCH.md)

The DS3 lead "fire hkb event `W_Event<animId>` at the hkbCharacter from behavior module +0x10 → +0x28" matches Sekiro 1.06 exactly [V]: `x = *(modules+0x28 + 0x10)`, `graph = *(x+0x28)` (`0xB41C40` / `0xFFE020`). It is the same final step the event module performs (`0xFF1DA0` → `0xFF1D10`), minus the three gates and the one-frame request slot.

- Call `0xFF1CD0(HkPair* p, const wchar_t* name) -> bool` with `p = {graph, x+0x38}` and `name = L"W_Event" + decimal(anim)` (the game uses `%04d`, so pad below 1000). [V] It returns false if the graph is null or the name is not an event of this graph (the index lookup gives -1, and `0xFF1D10` rejects idx < 0). So the return value is a free live check of whether an id exists.
- Or call `0x1001270(x, idx)` once the index is known.
- [I] It skips the event module's "busy/throw/flag 0x8000" gates. Those gates exist for a reason (throws, special states), so prefer Path A when it works and A′ when you want a definite yes/no.

Other leads checked:
- Slot `0x58` really is the event module in Sekiro [V]: the builder stores index 11 from `0xBD9070`, and the ghost's update dereferences it every frame.
- The request id is at event module **+0x10** in Sekiro [V] (not DS3 +0x20 or ER +0x18).
- SekiroTool `ForceAnimation 0x6D5A90(entityId, anim)` goes through the EMEVD entity lookup (`0x753860` → `0x756AE0` → `0x6D5B20`), then `chr+0x2118`. It fails for an unregistered ghost for the same reason as our path. `ChrIns+0x1A24` was not checked.
- The TimeAct "+0x20 = current anim" lead holds only for ring entry 0. The writer advances head (`+0xE8`) mod 10, so the newest entry is `(head+9)%10` [V]; `+0xEC` is the tail (read index) [V].

## § Ghost appearance (2026-10-22)

Scripts: `re/ghostlook.py`, `re/ghostlook2.py` … `re/ghostlook9.py` (headless IDA on the same dump; copies run from `F:\SekiroRE\scripts\`, because `-S` breaks on paths with spaces). Output: `F:\SekiroRE\out21` … `out29`. Static analysis only. RVAs are relative to the module base. Tags as above: **[V]** decompile, **[I]** inferred, **[G]** guess.

### Short answer

Neither `+0x22A1` nor the packed "look" value makes the ghost a phantom. The look comes from **ReplayGhostIns virtual slot 248** ("is replay ghost", `0xA39860`, always true; the PlayerIns version `0xA2B5B0` always returns false) and from the **chr type 3** that the plain factory hard-codes. Both feed the model's draw-entity flags when the model is built, and slot 248 also pushes a ghost-only shader value into the model every frame.

### `+0x22A0 … +0x22AD` on ReplayGhostIns [V]

All are written by the two ReplayGhostIns ctors (`0xA39280` plain, `0xA39100` data-backed). Only the ghost's own overrides read them.

| Offset | Type | Ctor value | Meaning |
|---|---|---|---|
| `+0x22A0` | u8 | 21 (plain ctor, `0x106D510`). The data ctor takes it from the replay data (`0x8E4450`). | Ghost "kind". 21 = none. Slot 130 (`0xA39620`) uses it to pick a model-param row from a table at `0x3B15968` (56-byte rows). 21 falls back to the normal ChrIns path `0x9F0720`. |
| `+0x22A1` | u8 | 1 if chr type is 3 or 11, else 0 | **Fade enable**, not translucency. When it is 0, the per-frame fade step `0xA39C10` returns immediately and never touches the opacity at `+0x22A8`. |
| `+0x22A4` | i32 | 0 | Fade direction: 0 = fade in towards 1.0, non-zero = fade out towards ≤ 0. Setter `0xA399E0(ghost, dir)`. |
| `+0x22A8` | f32 | 1.0 | **Opacity**. Slot 251 (`0xA395A0`) returns its absolute value (0.5 for chr type 11). The draw update multiplies the model alpha by it (see below). The fade step moves it by ±0.1 per update. |
| `+0x22AC` | u8 | 0 | Set by `0xA39870`, which also writes byte `+0x3B4` of the manipulator at `+0x58`. Read by slot 155. Not visual. |
| `+0x22AD` | u8 | cleared at the end of slot 9 | Read by slot 252. |

Slot 250 (`0xA397D0`) means "fading out" (`+0x22A4 != 0 && +0x22A1`). Slot 249 (`0xA39780`) means "faded out / gone": type 11, or alpha ≤ 0 while fading out, or alpha < 0. Slot 130 returns row 75 while the ghost is fading out.

Consequence: with `+0x22A1 = 0` the opacity stays at 1.0 (fully opaque), so the menu toggle cannot change the look. That matches what was seen. 1.0 is already the right opacity [V].

### The packed "look" value and `0x9F6780` [V layout, I meaning]

- `**(chr+0x120)` is the first dword of a 32-byte block built in the ChrIns ctor (`0xA16740`, then patched): `[0] = 1, [1] = 48, [2] = 23, [3] = 0, [4] = 0`. **Every ChrIns starts with `[0] = 1`**, which is also the value read on our ghost.
- `0x9F6780(chr, v)` stores `v` there, calls `PlayerCtrl` (`chr+0x50`) virtual slot 7 (`0x7F3480`) and zeroes a counter at `+0x2E0` of the ctrl's sub-object (`+0x358`, or `+0x120` if that is null).
- `0xBF1410` splits the replay's packed u32 into seven ints. The replay copies them to `PlayerGameData+0x08…+0x20` (the object returned by slot 94), in this order: `bits 0-1`, `bits 20-21`, constant 0, `bits 24-25`, `bits 26-27`, `bits 28-29`, `bits 30-31`. Only the first one (bits 0-1) goes to `0x9F6780`.
- Code that reads `[0]` compares it with 2 when picking a weapon/hand slot (`0xA2A9A0`, `0xA2AEB0`), so it looks like a stance or hand mode with values 0..3 [I, low]. Writing 0 made the ghost invisible (observed). **It has nothing to do with phantom shading. Leave it at 1.** None of the other 2-bit fields turned out to be a visibility or shader field.

### Chr type: `ChrIns+0x70` [V]

- The ghost-type argument of the factory/ctor ends up at `+0x28` of the ChrIns init block, and the ChrIns ctor (`0x9E7A60`) copies it to **`ChrIns+0x70`**. That field is the chr type, at the same offset as in DS3. The plain factory path `0xA057A0` always passes **3**. `0xA494A0(wcm, type, data)` takes the type as an argument.
- Getter: `0x9EEEC0(chr, &out)`. Setter: **`0x9F5170(chr, type)`**. The setter writes `+0x70`, then `0x9F64D0` recomputes the team type at `chr+0x74` (from NpcParam / player data) and refreshes a few player-only systems. `0x7C4220(PlayerGameData, type)` writes the type to `PlayerGameData+0x8C`. The respawn path `0xA2CD50` calls both.
- Property table at **`0x3B0A3B0`**: 14 rows × 20 bytes. Types out of range use the row at `0x3B0A4C8`. Each column has its own accessor [V]:

| Col | Accessor | Set for types | Best guess [I] |
|---|---|---|---|
| b0 | `0xBF1AE0` | 1 2 3 4 8 10 11 12 | not the local player / remote |
| **b1** | **`0xBF1B90`** (104 callers) | **3 4 10 11** | **replay-ghost type** |
| b2 | `0xBF1B30` | 0 8 | real player |
| b3 | `0xBF1B00` | 1 2 3 4 10 11 | phantom-like (multiplayer phantoms + ghosts) |
| b4 | `0xBF1A50` | none | — |
| b5 | (via table offset +5) | 0 5 6 7 8 9 12 13 | — |
| b6-b8 | `0xBF18A0`, `0xBF1A80`, `0xBF1AB0` | 0 8 | — |
| d12 | `0xBF19F0` | its own number for 3..12; 0 for 0 1 2 13 | written back as the chr type on respawn (`0xA2CD50`) |
| d16 | `0xBF18D0` | 8 for 0 1 2 13; otherwise its own number | — |

`0xBF1A20(type, x)` is true for type 8, and for types 5/9 when x == 4. Type 8 is the only non-ghost type whose columns match type 0 except for b0, so it probably means "a player, but not the local one" [I].

Where the ghost type changes the model [V]:
- The **model rebuild `0xA361C0`** is called from five PlayerIns paths, for example after equipment changes. It passes "b1 && the chr has no NpcParam override" as the 5th argument of the part loader **`0xE7B000`**. When that flag is set, the loader rewrites each part's resource id through a param lookup (`0x10CA960`/`0x10CA8C0`) and skips one load branch, so the ghost gets different part resources. The 6th argument is the opposite: "not ghost type", or local player, or slot 249.
- Physics/collision setup `0xBC3140` uses b1 to pick ghost collision groups (68 for everything). Not visual.

### Slot 248 "is replay ghost" and the model [V]

ReplayGhostIns differs from PlayerIns in these slots (`re/out18`): 0, 7, 9, 11, 14, 22, 25, 55, 130, 155, 221, **248-252**. The PlayerIns versions of 248-252 are `0xA2B5B0` (false), `0xA2AE70` (false), `0xA2B0D0` (false), `0xA28A80` (1.0) and `0xA34F90`. The ReplayGhostIns versions are `0xA39860` (**true**), `0xA39780`, `0xA397D0`, `0xA395A0` (opacity) and `0xA39BC0`.

1. **Model creation.** This happens in PlayerIns slot 9 (`0xA2C2B0`), which ReplayGhostIns slot 9 (`0xA398C0`) calls first. Slot 248, the raw chr type (compared with 0), "is local player" (`0x9F3430`: `WorldChrMan+0x88 == chr`) and slot 54 are packed into a draw-entity seed. The seed is used for **`chr+0x48` (NS_SPRJ::ChrModel, ctor `0x8CFFA0`)** and **`chr+0x2150` (NS_SPRJ::ChrAsmModel, ctor `0xE66D20`, base `0xE81EE0`)**. The base ctor creates the GX draw entity from it (`0xF137A0`, stored at model+0x10).
   - Seed byte 4 is a draw group: 6 = local player, 7 = other player, **8 = replay ghost** (7 + slot 248).
   - Bit 0x20 of seed bytes 0 and 2 means "not ghost".
   - Flags 0x02/0x04/0x10 are set only when the chr type is 0 (or slot 54 is true).
   - All of these are fixed once the draw entity exists.
2. **Every frame**, the PlayerIns draw update **`0xA2F780`** copies the colour/alpha block from `chr+0x48` to the ChrAsmModel and sets dirty bits in the qword at `ChrAsmModel+0x108`:
   - `+0x180` (f32) = `ChrModel+0x15C` × slot 251 (the ghost opacity), dirty bit 0x80.
   - **If slot 248 is true: `+0xD8` (f32) = −0.5 (constant `0x3B06608`), dirty bit 0x400.** The ChrAsmModel ctor sets it to 0.0, and nothing else in the model code writes it.
   - The model flush `0xE841D0` passes the dirty fields to virtual setters on the GX draw entity: `+0xD8` → slot 36 (`+0x120`); `+0x180` → slot 31 (`+0xF8`); `+0x98…+0xB7` → `0xF182A0`; `+0xB8…+0xD7` → `0xF18260`; bytes `+0xDC/+0xDD/+0xDE` → slots 35/45/46.
3. **The colour block that feeds the model.** `0x9F6B20` (per-frame ChrIns draw sync) copies `chr+0x1A68…+0x1A87` to `ChrModel+0x118`, the result of `0x9F8520(chr)` to `ChrModel+0x138`, and bytes `chr+0x1A5D/+0x1A5E` to `ChrModel+0x175/+0x174`. ReplayGhostIns slot 25 (`0xA39D60`) and the PlayerIns ctor (non-local branch) both fill `chr+0x1A68…+0x1AA7` with the all-ones "unset" pattern (`0x3D88880/90`). So this block is the same for a player puppet and our ghost.
4. **Model-param modifier.** `0x9F6B20` also calls ChrIns slot 130 and passes the returned row to `0xB9AF40`. That function belongs to the FdpChrModelParamModifierModule at `modules+0xD8`, which owns the `ExParam_Phantom*` names (edge/front/diff/spec/light colour, edge power, subtract flags). Our plain ghost has `+0x22A0 = 21`, so slot 130 already falls back to the normal ChrIns path, except while fading out (row 75) [V]. This path therefore does not cause the look unless a fade-out is running. The normal path takes the row from `chr+0x1F88` (forced, −1 = none), then `chr+0x1A60` (from SpEffects via `0xBFD720`, −2 = none), then NpcParam/PlayerGameData.

I did not trace what `+0xD8 = −0.5` and draw group 8 do inside GX. PhantomParam exists in this build (param name table entry `0x3B16500`, enum 0x3A), and the renderer has "Phantom"/"PhantomLite" passes (`0x1CCC20`, `0x1CE070`). [I, medium] These per-model values choose the phantom pass: the draw group and seed flags at creation, and the −0.5 value every frame.

### Runtime recipe (ranked; for live toggles)

1. **Make slot 248 return false for our ghost before its model is created.** [I, high that this is the main switch; the timing is unverified.]
   - Clone the vtable per object. Copy the qword at RVA `0x2A2D188` (the RTTI locator, so `rtti::class_name` keeps working) and the **253 slots** from RVA **`0x2A2D190`** (the table ends at `0x2A2D978`).
   - Patch slot 248 (`+0x7C0`) to base + `0xA2B5B0`, then point `*(ghost+0)` at the cloned slot 0.
   - Do this right after `0xA493C0` returns. The factory calls only slot 14 synchronously (`0xA057A0`), and slot 9 (model creation) runs later, so swapping in the same frame is probably early enough. Not verified.
   - Optionally also patch 250 → `0xA2B0D0`, 251 → `0xA28A80` (opacity always 1.0) and 249 → `0xA2AE70`.
   - Side effect: the lock-on/target code `0xA4E8E0` and `0x9D1C00` also check slot 248, so the ghost may become lock-on-able [I].
2. **Undo the per-frame ghost shader value.** This works even if the model already exists. Each frame, after the game's chr update, write f32 `0.0` to `*(ghost+0x2150) + 0xD8` and OR `0x400` into the u64 at `*(ghost+0x2150) + 0x108`. It is only useful together with 1: otherwise the game writes −0.5 again every frame, and the result depends on which write lands last. [I, medium]
3. **Change the chr type, then rebuild the parts.** Call `0x9F5170(ghost, 8)` (or 0), then run `dress_like` (`0x9CCBA0`) again so that `0xA361C0` reloads the parts without the ghost-type flag. [V for the setter and the loader flag; I that dress_like triggers `0xA361C0`.]
   - Type 8 is the safest non-ghost type: b1/b3 = 0, b2 = 1, and it is not the local player.
   - Type 0 may make the ghost look like "the" player to code that checks the type instead of `WorldChrMan+0x88`.
   - Other b1-gated behaviour will change too (104 call sites). Collision groups are fixed at creation, so they stay ghost-like.
   - This does not rebuild the draw-entity seed.
4. **Keep the existing values**: look = 1 (do not call `set_look(0)`), `+0x22A1 = 0`, `+0x22A4 = 0`, `+0x22A8 = 1.0`. This gives a fully opaque ghost with no fade, but it does not remove the phantom shading. [V]
5. **Last resort**: spawn through `0xA494A0(wcm, 8, data)` so that type 8 is already set before slot 9 runs. The data-backed ghost deleted itself when its replay was empty (see ghost.rs notes). It therefore needs a replay with at least one frame, or the deletion check (slot 249 / "finished" flag) has to be neutralised. [I]

### Confidence summary

| Claim | Level |
|---|---|
| `+0x22A1` = fade enable, `+0x22A4` = direction, `+0x22A8` = opacity (1.0 = opaque) | V |
| look `[0]` defaults to 1 for every ChrIns and is not a shader selector | V (layout) / I (meaning) |
| `ChrIns+0x70` = chr type = the factory's ghost-type argument; the plain factory passes 3 | V |
| Setter `0x9F5170`, table `0x3B0A3B0`, column meanings | V (code) / I (names) |
| ReplayGhostIns slot 248 is always true, PlayerIns always false; it feeds draw group 8 and `ChrAsmModel+0xD8 = −0.5` | V |
| These values select the phantom render pass | I (medium); the GX side was not traced |
| A vtable swap right after spawn happens before slot 9 runs | unverified |
| `dress_like` triggers the part rebuild `0xA361C0` | I |

---

## § Ghost animation, part 2 (2026-10-22)

Follow-up to the live test where the raise hook on `0xFF1D10` saw almost no events for the player's graph.
Scripts: `re/ghostanim6.py` … `re/ghostanim11.py` and `re/ga2_*.py` (headless IDA, run on a private copy `F:\SekiroRE\anim2db\quick_anim2.i64` so the shared IDB is never locked). Output: `F:\SekiroRE\out30` … `out45`. Static analysis only. RVAs are relative to the module base. Tags as above ([V] decompile, [I] inferred, [G] guess).

### Why the 0xFF1D10 hook misses the player [V]

- The player's actions come from the c0000 HKS script. Its native `hkbFireEvent` (`0x13F92C0`, registered next to the string reference at `0x13F78FC`) looks the event name up in the character's project string data and **pushes `{idx, 0, sender}` (24 B) straight into the graph event queue** (`graph+0xD0`, or a context queue). It grows the queue with `0xFF20D0` itself and never calls `0xFF1D10`. That is why the hook sees enemies' C++-raised events but not the player's HKS ones.
- `0xFF1D10` has only 6 code callers (`0xAE90B0`, `0xB4E9C0`, `0xFF1CD0`, `0xFF1DA0`, `0x1001270` ×2). `0xFF20D0` (queue grow) has 21 callers, almost all Havok-internal (`0x11E…`–`0x14B…`), plus the HKS binding.
- Event indices are project-global, not per graph [V mechanism / I equality]. `0xFF0DF0(int* out, const wchar_t* name)` formats `L"W_%s"` and looks it up in a map reached from singleton `0x3D92828` (`**(s+8)` → `0x13A6AF0` → `+0x18` → `+0x28`). HKS uses the same kind of map from the character's data. Two c0000 characters therefore share indices, which matches the replay code.

### The per-event notification every handled event goes through [V chain, I coverage]

| RVA | Signature | Notes |
|---|---|---|
| `0xB42390` | `void notify(u32 eventIdx, u8 flag, ChrIns* chr)` | Callback registered at boot by `0x1042480` into singleton `0x3D92828` (`lea rdx` at `0x1042769` → `0x1015E60`, which forwards to the delegate object at `singleton+0x20`, vtbl `+0x50`). The same block registers the TimeAct ring writer `0xB5C920`. Returns at once if `chr == 0`. Calls `0xB44590(*(*(chr+0x1FF8)+0x28), idx, flag)`, then `0xB49270`, then sets `*(*(chr+0x1FF8)+0xA8)+0x44 = 0x0101`. |
| `0xB44590` | `void bm_on_event(SprjChrBehaviorModule* bm, u32 eventIdx, u8 flag)` | Records into the sync recorder `R = bm+0x60` via `0xFED450(R, idx)` and sets `bm+0xB22 = 1` if it recorded. Then two debug helpers. **Only code caller: `0xB42390`.** |
| `0xFED450` | `bool R_record(R*, int eventIdx)` | Finds the active graph's transitions that use this event. For flagged ones (`transition+0x42 & 0x200` picks `0xFEEBC0`, otherwise `0xFEEB20`) it adds `{node, transition+0x34}` and pushes into `R+0x7F0`. **Only code caller: `0xB44590`.** |

Coverage argument [I, high]: remnant/replay ghosts reproduce the player's attacks. The recorder's events come only from `R`, and `R`'s events come only from `0xFED450` ← `0xB44590` ← `0xB42390`. So `0xB42390` must run for HKS-fired player events. The invoking site inside hkb event handling was not found statically (it is an indirect call through the delegate), and its thread is unknown [G]. It is probably inside the behavior graph update, which may run on a worker thread.

Hook data:
- `0xB44590` prologue `48 89 5C 24 08 | 48 89 74 24 10 | 57 | 48 83 EC 20` [V]. The first instruction is exactly 5 bytes and not RIP-relative, so a 5-byte jmp relocates it verbatim and returns to `+05`. `rcx = bm`, `edx = idx`, `r8b = flag`.
- `0xB42390` prologue `4D 85 C0 | 0F 84 93 00 00 00 | 48 89 5C 24 08 …` [V]. The `jz rel32` at `+03` would have to be re-encoded if hooked there, so prefer `0xB44590`.

### The sync recorder `R = SprjChrBehaviorModule + 0x60` [V layout unless tagged]

`0xFED630(R, hkChr)` initialises it from the behavior module init `0xB438A0`. Every frame `0xFED670(R)` runs from the behavior pre-update `0xB45440` (← `0x7DEFA0` ← PlayerIns update `0xA37560`, so also for ReplayGhostIns). `0xB45440` stores the result in `bm+0xB20` ("sync state changed"), copies `bm+0xB22` into `bm+0xB21` and clears `bm+0xB22`.

| Offset in R | Content |
|---|---|
| `+0x000` | `hkChr*` (same object as `bm+0x10`; `*(+0x28)` = graph) |
| `+0x008` | list A, ≤16 entries × 12 B: `{i32 node, i32 eventRef, u8 f8, u8 f9, u8 fresh}`, count u64 at `+0x0D0`. `0xFED670` rebuilds it every frame from the **currently active states**. `fresh = 1` when the transition into the state was fired by an event recorded since the last update. |
| `+0x0D8` | list B, same shape, count `+0x1A0` |
| `+0x1A8` | list C = previous frame's A, count `+0x270` |
| `+0x278` | "set variables", ≤32 entries × 32 B (`+0x04` type; idx via `0xFF06A0`), count u32 at `+0x678`. `0xFEE110(R, varIdx)` fills it from the variable bindings of active nodes (`0xFEDF70`). Variables in the lists below are excluded. |
| `+0x680` | var list 2, u32 idx × n, count `+0x708`: `MoveType`, `AddBlend00..09` (c0000 only) |
| `+0x710` | **var list 1**, count `+0x758`: `ArtsTransition`, `MoveSpeedIndex`, `MoveDirection` (c0000 only; built by `0xFEFB50` with `0xFF4890`) |
| `+0x760` | var list 3, count `+0x7E8`: `BlendPart01..16` (c0000 only; sent as a bitmask by `0xFEC1D0`, applied by `0xFEC040`) |
| `+0x7F0` | DL vector of event records pushed by `0xFED450` this frame (cleared by `0xFED670`) |

Readers, all [V]:
- `0xFED3D0(R*, DlVec* out)` (thunk to `0xFEB320`). For each list-A entry that passes `0xFEBCB0` (a filter on `f8`/`f9`, controlled by config bytes `0x3B066E7`/`0x3B066E8`), it resolves the entry's name (`0xFEA900`) to a **u16 sync-event id** (`0x10158F0`, dictionary in singleton `0x3D92578+0x10`). It appends a u32 `{u16 syncId, u8 fresh, u8 0}` and returns false if `R+0` or its graph is null.
- `0xFEC140(R*, DlVec* out)` (→ `0xFEAF40`) returns the same ids; the ghost side uses it for the comparison.
- `DlVec` = `{ IAllocator* alloc; u8* begin; u8* end; u8* cap }` (32 B). The game creates it as `{heap, 0, 0, 0}` with heap `*(base+0x3D9CE50)` and frees `begin` through `alloc->vtbl[0x68/8](alloc, begin)`.

### Applying sync events on a ghost [V]

`0xB4E9C0(SprjChrBehaviorSyncModule* bsync, DlVec* list, u8 unused)`, with bsync = `*(chr+0x1FF8)+0x30`:
1. Returns if `*(bsync+0x10) == 0`, or if the module at `modules+0x88` is in a special state (`0xBDFB70` / `0x802A60`, presumably a throw).
2. `0xB4F510` reads only `list->begin/end`. It gets the ghost's own current ids (`0xFEC140` on the ghost's R) and keeps an id if the ghost is **not already in that state, or the id's `fresh` byte is set**.
3. Calls `0xB411C0(bm, &kept)`. Then for each kept id: `0xFE9240(id)` → name, `0xFF0DF0` → event idx (`W_<name>`), `0xFF1D10(pair, idx)`. Sets `bsync+0x18 = 1` if it raised anything.

So the replay's event channel is a **state diff**. It sends the set of active sync states every frame, and the receiver jumps only to the states it is not in. A lost packet costs latency, not correctness.

### Variable blocks [V]

`HkBehAllVariable` is 0x25D0 = 9680 B, ctor `0x8D4800(void*)`, no heap and no dtor needed. It holds five sorted fixed maps of `CSRegularSyncProperty` entries:

| Block | Offset | Capacity | Entry | Count at | Apply fn | Setter |
|---|---|---|---|---|---|---|
| s8 (bool/int8) | `+0x58` | 64 | 24 B | `+0x668` | `0xFF85E0` | `0xFF3A60(h, u8)` |
| s16 | `+0x670` | 32 | 24 B | `+0x980` | `0xFF8850` | `0xFF3A40(h, i16)` |
| s32 | `+0x988` | 64 | 24 B | `+0xF98` | `0xFF86A0` | `0xFF3A50(h, i32)` |
| f32 | `+0xFA0` | 128 | 24 B | `+0x1BB0` | `0xFF8520` | `0xFF3A30(h, f32 in xmm1)` |
| vec4 | `+0x1BB8` | 64 | 40 B | `+0x25C8` | `0xFF8760` | `0xFF3BD0(h, vec4*)` |

- Entry layout: `+0x00 i16 key` (the variable index the setter receives), `+0x08` vptr, `+0x10` value, `+0x14` u32 send count (set to 5 on change). The count is a u64 at `map + 8 + cap*stride + 8` (the offsets above assume 8-byte alignment). Entries are kept sorted by key, because lookups use binary search.
- Fill (sender): `0xFECE80(R*, all*)` adds the R "set variables" with current values, and `0xFEC790(R*, all*)` adds var list 1 with current values. Both return bool. The recorder calls them in that order and skips `0xFEC790` when `0xFECE80` returns false.
- Apply (receiver): `0xFFEBB0(hkChr*, all*)` walks the five maps and calls the typed setter for each entry.
- Low-level variable API, all [V]:
  - `0xFFE740(hkChr* x, VarH* out, int idx)` builds `VarH = { i32 idx; pad; graph = *(x+0x28); x+0x98 }` (24 B).
  - `0xFF3CC0(graph, idx) -> i32 type` returns the Havok variable type, or `-1` if invalid: 0 bool, 1 int8, 2 int16, 3 int32, 4 real, 7 vector4 (the only cases the sync code handles).
  - Getters: `0xFF3710(VarH*) -> u8` (bool/int8), `0xFF36F0 -> i16`, `0xFF3700 -> i32`, `0xFF36D0 -> f32` (in xmm0).
  - Float setter `0xFF3570` (thunk `0xFF3A30`). It does nothing if the idx is in the override map at `*(x+0x98)+8`. Otherwise it maps idx through `*(*(graph+0x98)+0x100)+0x28` and writes `*(*(*(graph+0x98)+0x118)+0x18)[internal] = value` (an array of 32-bit words). It needs `0x13779F0(*(graph+0x98)) != 0`.
  - **Name to index: `0xFF4890(int* out, const wchar_t* name, graph) -> int*`**. The behavior module uses it to cache its variable indices. [I] `*out = -1` if the name is not found.

### Locomotion: what actually drives the walk/run blend [V mechanism, I/G where tagged]

- The behavior module caches 91 variable indices at **`bm+0x980`** (fixed vector of i32, count at `bm+0xAF0`). `0xB438A0` builds it from the name table at `0x3B08E30` (16-byte entries `{wchar_t* name, u8 enable_c0000, u8 enable_other}`). The first entries are k=0 `IsLockon`, 1 `MoveAngle`, 2 `MoveSpeedLevel`, 3 `MoveDirection`, 4 `TurnAngle`, 5 `IsEnableFootIk`, followed by AddBlend*, the Jump/Attack/Kick angles, IK, slope and face entries. Index = `*(i32*)(bm+0x980+4k)`, or `-1` if disabled.
- Every frame `0xB45440` → `0xB49D30` → **`0xB4AFD0`** (skipped when `0x9F36C0(chr)` is true, see below) computes:
  - input vector `v = *(P+0x10)`, with `P = 0x9EFEA0(chr) = *(*(chr+0x50)+0x358)`, or `*(*(chr+0x50)+0x120)` if that is null. v is in world space and |v| is the stick level;
  - `MoveAngle` (k=1) = the angle of v in the chr's local frame, in degrees, cached at **`bm+0xBB0`**;
  - `MoveSpeedLevel` (k=2) = |v|, or 2.0 when bit 2 of `*(*(chr+0x1FF8)+0x80)+0x184` (dash) is set. It is clamped by a walk-mode state (`*(*(*(chr+0x50)+0x50)+0x520)`: 1 → ≤1.0, 2 → ≤0.1, 3 → 0) and cached at **`bm+0xBAC`**;
  - `MoveDirection` (k=3) = a u8 from `(*(chr+0x50))->vtbl[0xC0/8]()`.
  - The level is forced to 0 when `*(chr+0x1F40) >= 0x80`, or when config byte `0x3D7A375` is set and the chr is neither local nor "remote" [I].
- `0x9F3430(chr)` = `chr == *(WorldChrMan+0x88)`, i.e. the local player [V]. `0x9F36C0(chr)` = `vtbl[53]() && !local && !vtbl[54]()` [V]. PlayerIns and ReplayGhostIns share slot 53 (`true`) and slot 54 = `0xA2B4E0`, which tests `(*( *(chr+0x2000) or 0x7C39E0() ) + 0x10) - 10000000 <= 0x27`. So whether our ghost counts as "remote" (and skips the writer) depends on its handle id [G, check live].
- **How replays make ghosts walk** [V]: ReplayManipulator slot 41, `0x7F5340(manip, vec4* frameDelta)`, stores the delta at `+0xE0` and writes **`manip+0x10` = horizontal unit direction × (1.0 if |delta|·30 ≥ 2.5, else 0.5)**, also at `+0xC0`. The ghost's own `0xB4AFD0` turns that into `MoveSpeedLevel`/`MoveAngle`, and HKS derives the rest. The replay frame variables only add `ArtsTransition`, `MoveSpeedIndex` and `MoveDirection` (var list 1) and the transition-bound variables.
- Our ghost's ReplayManipulator never writes `+0x10` while it has no frames: `0x7F48D0` (slot 9) returns early if `*(manip+0x380) == 0` or the frame count is `≤ 0` [V]. Base slots 5 (`0x7E3D10`) and 8 (`0x7E3510`) zero `+0x10`/`+0xC0`, but they are reset/init functions and how often they run is unknown [G].

### Replay frame and bandwidth [V unless tagged]

- The ReplayRecorder record function `0xA3BC90` (← `0xA38C10`, `0xE8D900`) samples a frame once 1/15 s has accumulated, or immediately on a state change. The frame object sits at `recorder+0xF0` with 0x27E0 B of payload: the events DL vector at `frame+0x1F0`, `HkBehAllVariable` at `frame+0x210`, and flags at `+0x27E0` (= `bm+0xB21`) and `+0x27E1` (= `bm+0xB20||bm+0xB21`). `0x8D7000(frame, BitWriter*, 1024)` serialises it into a `{u32 size; u8[1024]}` record. The playback decoder `0x8DCF90` returns the size, and `0x7F4C90` drops frames larger than `0x400`. **So one frame is ≤ 1 KB, and ≤ 15 KB/s at 15 Hz.** It is usually far less, because values are sent only while their send count is non-zero.
- A leftover network-style packet also exists (fill `0xB4F790`, apply `0xB4EF70` + `0xB4EF10`), used when config byte `0x3B066E6` is set: `+0x10` events DL vector, `+0x30` `HkBehAllVariable` (set variables only), `+0x2600` u32 BlendPart bitmask, `+0x2604/+0x2605` flags.
- Building a real frame object needs its constructor (not identified) and the bit writer. **Not recommended**: the same content is available from `R` with three plain calls.

### Confidence summary

| Claim | Level |
|---|---|
| HKS `hkbFireEvent` pushes to the graph queue directly, bypassing `0xFF1D10` | V |
| `0xB42390 → 0xB44590 → 0xFED450` is the only path into the sync recorder | V (exhaustive xref + byte scan) |
| `0xB42390` runs for every event the chr's graph handles, HKS ones included | I (high, by elimination) |
| Event indices are project-global (same on player and ghost) | I (high) |
| R list A = active sync states + fresh flag; `0xFED3D0` → u16 sync ids | V |
| `0xB4E9C0` applies a state diff (raises only missing or fresh states) | V |
| `HkBehAllVariable` layout, setters/getters, `0xFF4890` name→idx | V |
| The walk/run blend comes from the manipulator move vector `+0x10`, read by `0xB4AFD0` | V |
| Our ghost keeps a vector we write to `manip+0x10` at FrameBegin until `0xB4AFD0` reads it | G (check live: the ghost's `bm+0xBAC` should follow it) |
| The ghost is skipped by `0xB4AFD0` (`0x9F36C0` true) | G (depends on the handle id) |
| Thread of `0xB42390` | G |

---

## § Solid ghost (2) (2026-10-22, pass 3)

Follow-up to the live result "vtable clone with slot 248 → `0xA2B5B0` + `ChrAsmModel+0xD8 = 0` every frame + optional type 8 + re-dress: still a phantom".
Scripts: `re/re3_job.py` (generic job runner; job list in `F:\SekiroRE\scripts\re3_job.txt`), run on a private copy `F:\SekiroRE\re3db\quick_re3.i64`. Output: `F:\SekiroRE\out50` … `out79`. Static analysis only. Tags as above.

### Short answer

The phantom look most likely does **not** come from slot 248 at all, but from the **PhantomParam row that the game applies to the ghost every frame**, selected by **chr type** (3). Slot 248 and the draw group only add a second, smaller difference. Fix: make the ghost's vtable **slot 130** return what the local player's slot 130 returns. [I, high for the mechanism; the visual effect of each part is unverified]

### Where the phantom shading comes from [V chain, I meaning]

- Per-frame draw sync `0x9F6B20` (called from the PlayerIns draw update `0xA2F780`, so it runs for our ghost) calls **vtable slot 130** (`+0x410`). If the result is `>= 0` it passes `{0, row, 0.0}` to `0xB9AF40`, the apply function of the FdpChrModelParamModifier module at `modules+0xD8`. That function looks the row up in **param #58** (PhantomParam; the earlier note had enum 0x3A = 58) and pushes the ExParam_Phantom* values (edge/front/diff/spec colour, edge power, …) into the modifier. If slot 130 returns `< 0`, nothing is pushed that frame.
- The modifier keeps an entry only while it is refreshed: `0xB9BD90` (per-frame) drops the weight of every entry not touched this frame to 0 (blend time 0 → immediately). So returning `-1` from slot 130 removes the phantom shading on the next frame. [V]
- ReplayGhostIns slot 130 = `0xA39620`. With kind `+0x22A0 == 21` (our plain ghost) it calls the normal ChrIns version **`0x9F0720`** (also PlayerIns slot 130, vtable entry RVA `0x2A2B748`). Priority inside `0x9F0720` [V]:
  1. `i32 chr+0x1F88` if `>= 0` (forced row),
  2. `i32 chr+0x1A60` if `!= -2` (from SpEffects; rewritten every frame by `0xBFD720` just before slot 130 is called, so writing it is useless),
  3. NpcParam row (players have none),
  4. otherwise a row from **param #72** keyed by **`chrType + 10000*sub`** (key builder `0x9F0C30`; `sub` comes from vtable slots 200/195, 0 when not in a multiplayer state). If `u8 chr+0x1FA9 != 0`, the key is taken from `i32 chr+0x1FAC` instead. From that row one of four PhantomParam ids is picked (row dwords at `+0x04/+0x4C/+0x50/+0x54`) by a 0..3 level computed from PlayerGameData (`slot 55` → byte `+0x103`, `0x7C2F20`, thresholds 10/30/80 in `0x7C1430`).
- So a chr-type-3 ghost gets param-72 row 3 (the remnant/phantom row), type 8 gets row 8 (very likely also a phantom row in this DS3-derived table), and only type 0 gets the local player's row. This explains why the type-8 test changed nothing.

### Recipe A (recommended): slot 130 override [I, high]

We already clone the ReplayGhostIns vtable per ghost (`ghost.rs::make_solid`). In the same copy, replace **slot 130** (byte offset `+0x410` from slot 0) with a function of our DLL:
- `extern "C" fn ghost_phantom_row(chr: usize) -> i32` (x64 Windows: one calling convention, so this matches the game's `__fastcall`).
- Body: if the local player exists (`*(WorldChrMan+0x88)`), call **the player's own slot 130** (`*(*(player)+0x410)`, normally `0x9F0720`) with the player and return its result. Otherwise return `-1`.
- Effect: the ghost carries exactly the player's model-param row (or none). The game thread calls it once per frame per ghost; it is a plain read.
- No timing issue: it works on an already built model (applied per frame).
- Keep slot 248 → `0xA2B5B0` (removes the per-frame `ChrAsmModel+0xD8 = -0.5` write). `solid_shader_tick` then becomes unnecessary once the clone is in place before the first draw update.

Data-only alternatives (no code pointer), weaker:
- Each frame, `i32 ghost+0x1F88 = player_row` when `player_row >= 0` (forced row; read `player_row` by calling the player's slot 130). Does not help if the player's value is `-1`.
- Once: `u8 ghost+0x1FA9 = 1`, `i32 ghost+0x1FAC = 0` (key of the host row). [I] Writer of these two fields was not found, so another system could reset them.

### Answers to (a)–(d)

**(a) Factory internals [V].** `0xA493C0(WCM)` is `0xA077E0(WCM+0x58)`: find a free 56-byte slot in the ghost set, build the handle with `0xC33180(&h, *(*(WCM+0x10)+0x18)+1, slot)`, then **`0xA057A0(lock, slotEntry, handle)`**: allocate 8880 B (heap `0x3D9CE70`), **`0xA39280(mem, slotEntry, handle, chrType=3)`**, call vtable slot 14 (`0xA39750`: manipulator setup `0x7DE430` + `0xA34C90`), set `+0x1A3C = 1`. There is no data-less path that takes a type: the type is the 4th argument of the inner ctor `0xA39280`, hard-coded to 3 by `0xA057A0`. Calling `0xA39280` ourselves would also need the slot registration done by `0xA077E0`, so it is not recommended. Setting `+0x70` right after the factory is equivalent for everything that reads the type later.

**(b) .rdata patch during the factory [V].** Useless: model creation (vtable slot 9 → PlayerIns `0xA2C2B0`) is **not** called inside `0xA493C0`. It runs later from the chr-set update (caller not identified). Our per-object clone made right after the factory is therefore early enough *if* slot 9 runs in a later frame. Live check: **`u8 *(ghost+0x48) + 0x23`** (ChrModel draw group; 6 = local player, 7 = other player, 8 = replay ghost). 7 means the clone was in time. Chr type alone does not force group 8: group = 6 if local, else `7 + slot248()`, unless a module (`*(modules+0x18)`, `0xBD57F0`) or the slot-47 param overrides it. Chr type ≠ 0 only clears seed flags `0x02/0x04/0x10` (and with them the second draw group at seed byte 4).

**(c) Rebuild after changing type/slot [V].** Writing equip ids (`0x9CCBA0`) only stores the id and refcounts the part; it does **not** call the part loader. The loader `0xA361C0(chr)` is reached through vtable **slot 21** (`0xA30A50`), and slots 8, 27, 33, 34. Call `ghost.vtbl[21](ghost)` (or `0xA361C0(ghost)` directly) on the game thread after `0x9F5170(ghost, 8)`: the loader's 5th argument ("ghost type, rewrite part ids through a param") becomes 0 and the parts reload. Nothing rebuilds the draw-entity seed or the ChrModel/ChrAsmModel objects except slot 9 (which allocates new ones at `+0x48` / `+0x2150`). Do not call slot 9 on a live chr.

**(d) Rewriting the draw group on existing models [I, low].** Seed byte 3 (draw group) and byte 4 are turned into **graphics-param row pointers**, not plain numbers: `0xF3A830(entity+0xE0, group)` stores `0x108E2A0(*(base+0x3D95B68), group)` at **`entity+0xE0+0x50`** (and byte 4 at `entity+0x190+0x50` when seed flag `0x200` is set). The ChrModel copy of the byte is at `ChrModel+0x23`. A post-hoc swap would mean writing `0x108E2A0(*(base+0x3D95B68), 7)` into `drawEntity+0x130` of both draw entities. Where the entity pointer sits inside the model (`+0x10` holder, `0xE826C0` / `0xE876F0`) was not confirmed. Use only if A alone leaves a visible difference.

### Ranked runtime recipe

1. **Slot 130 override** (Recipe A) in the cloned vtable, plus the existing slot 248 swap. Log once, live: `ghost.vtbl[130](ghost)` before the override vs `player.vtbl[130](player)`. If they differ, this is the phantom source.
2. Log `*(u8*)(*(ghost+0x48)+0x23)`. If it is 8, the clone came after slot 9: either clone earlier (no earlier point exists outside the factory) or apply (d).
3. Optional: `0x9F5170(ghost, 8)` then `ghost.vtbl[21](ghost)` to reload parts without the ghost-type part rewrite.
4. Keep look = 1, `+0x22A1 = 0`, `+0x22A8 = 1.0`.

### Confidence summary

| Claim | Level |
|---|---|
| Slot 130 → `0xB9AF40` → PhantomParam (#58) row applied every frame; `< 0` = nothing | V |
| Unrefreshed modifier entries drop to weight 0 at once | V |
| Row comes from param #72 keyed by chr type (3 → ghost row) | V (code) / I (row content) |
| This row is what makes the ghost look like a phantom | I (high) |
| Slot 9 (model creation) is not called inside the factory | V |
| Equip-id writes do not reload parts; slot 21 / `0xA361C0` does | V |
| Draw group → graphics param row at `entity+0x130` | V (ctor) / I (effect) |

---

## § Enemy AI authority (2026-10-22, pass 3)

### Short answer

The game still contains the **DS3 co-op enemy sync**, complete, and it works through in-process tables that need no network session:
- Every EnemyIns owns **two manipulators**: `chr+0x2000` = ComManipulator (the AI) and `chr+0x2008` = **NetAIManipulator** (remote-driven, no AI). `chr+0x58` points at the active one.
- On the host, the AI manipulator writes the enemy's movement, behavior-sync and action state into **per-netId records in `WorldChrMan+0x630`** every frame.
- On a client, the NetAIManipulator reads the same kind of records and drives the enemy: position/rotation through ChrCtrl movement, hkb sync states through `0xB4E9C0`, variables through `0xB4EB70`, AI action inputs through its own getters.

So the recipe is: copy the host's records into the client's table and switch the client enemy to its NetAIManipulator. [V for every function named below; I where tagged]

### Which manipulator is active [V]

- EnemyIns ctor `0xA11F60`: `+0x2000` = ComManipulator (`0x9EAEA0`, ctor `0x7E4010`). If **`u16 chr+0x78 != 0xFFFF`** (the netId, from the init block `+0x44`), it also allocates a NetAIManipulator (1104 B, ctor `0x7E9F10`) at **`+0x2008`** and stores the netId in it (`0x7EA4C0`, `manip+0x340`). `chr+0x58` starts as the ComManipulator.
- **EnemyIns vtable slot 37 = `0xA14B50(chr, bool forceLocal)`** (vtable RVA `0x2A27F28`, 248 slots, RTTI locator at `0x2A27F20`). It picks the ComManipulator if `forceLocal`, or if the ownership table says "owned here" (`0xA4A650(WCM, netId)`), or if **`u8 chr+0x2040`** (slot 175) is set. Otherwise it picks the NetAIManipulator. On a change it calls the new manipulator's slot 6 (activate, with current pos/rot) and rebinds the ChrCtrl (`0x7DE3D0(ctrl, manip)` writes `ctrl+0x120`).
- **WorldChrMan's chr loop `0xA4DB20` calls slot 37 every frame** with `forceLocal = (session member count <= 1) || 0x9F33F0(chr)`. The count is `0x8E3E60` = `(*(S+0x168) - *(S+0x160)) / 56` with `S = *(base+0x3D948E8)`. Offline it is 0, so every enemy is forced back to the AI every frame. A one-time switch does not stick.
- Ownership table entries start at `-1` = "not owned here" (`0xA19A50`); only the online receive path `0xA1DD00` sets them to 1. Offline, therefore, `forceLocal = 0` is enough to select NetAI.

### The sync table `T = *(WorldChrMan+0x630)` [V]

Created unconditionally in the WorldChrManImp ctor `0xA46B80` (so it exists offline). Blocks are added per loaded map block by `0xA1BB50` (from `0xA4B280`).

| Where | Content |
|---|---|
| `T+0x08` | i32 block count |
| `T+0x10 + 8*b` | block `B` for `b = netId >> 11` (null if not loaded) |
| `B+0x10` | i32 entry count; `i = netId & 0x7FF` must be below it |
| `B+0x18` | i16[n] ownership (`>= 0` = owned here) |
| `B+0x20` | **movement records**, 28 B each |
| `B+0x28` | **behavior records**, 344 B each |
| `B+0x38` | **u16[n] flags**: `0x01` movement pending, `0x02` movement written (host), `0x04` behavior pending, `0x40` action pending, `0x100` movement consumed |
| `B+0x40` | **action records**, 48 B each |

Accessors (WCM wrappers): movement `0xA4B7E0(WCM, netId)` (no flag check), behavior `0xA4B740(WCM, netId)`, action `0xA1B950(T, netId)`. Consumers (NetAI side) check and clear the pending bits: `0xA4B7B0` (bit 1), `0xA4B710` + `0xA4B770` (bit 4), `0xA1B8F0` + `0xA1B9A0` (bit 0x40). netId of a chr: `0xA4CE60(WCM, chr)` = `(blockSlot << 11) | index`, computed from the chr's map block, so it exists offline [I, high]. It is local to each game, so each side uses its own enemy's netId.

### Record layouts [V]

Movement (28 B), written on the host by ComManipulator slot 11 (`0x7E54F0`):
- `+0x00` f32 x, y, z: world position (ChrCtrl position, `0x7DCF20` → physics module `modules+0x68`).
- `+0x0C` f32 ×3: rotation (the first 3 floats at physics module `+0x70`; the receiver wraps the difference by 2π, so these are Euler angles, y = yaw).
- `+0x18` 4 B unused.

Behavior (344 B), refilled by the host only when the sync state changed (`bm+0xB20 || bm+0xB21`, `0xB42190`):
- `+0x000` u32[16]: sync entries `{u16 syncId, u8 fresh, u8 0}`, exactly what `0xFED3D0(bm+0x60)` produces.
- `+0x040` 32 × 8 B: `{u32 hkbVarIndex, u32 rawValue}` (from `0xFED410` count / `0xFED3E0` + `0xFF0630`). Applied by `0xFF05D0` + `0xFFF040`.
- `+0x140` u8 event count (≤16), `+0x141` u8 var count (≤32), `+0x142` u8 = 1, `+0x143` u8 = `bm+0xB21`.
- `+0x144` u32 BlendPart mask (`0xFEC1D0`).
- `+0x148` i32 AI target mode: 0 none, 1 chr (`u16` netId at `+0x14C`), 2 position (f32 ×3 at `+0x14C`).

Action (48 B): two 24-byte objects from ComManipulator slots 20/21 (`+0xA0/+0xA8`), encoded by `0xB0E5B0`. On the NetAI side they are decoded by `0xB0E0C0` into `manip+0x3A0/+0x3E0` and returned by NetAI slots 20/21, which the ChrCtrl/HKS reads as the enemy's action inputs. [I: these are the AI's "pad" action requests that start attacks]

### What the NetAIManipulator does each frame (`0x7EA5C0`, slot 9) [V]

1. Movement record pending → `0x7EB2A0`: target pos `manip+0x350`, target rot `+0x360`; velocity `+0x370` = (target − current) / (time since the last record × 1.1), angular velocity `+0x380`. It snaps (`+0x390`/`+0x391`) when the implied speed exceeds `manip+0xF0`, when the time since the last record is ≤ 0 or > 2 s, or when the yaw error is > 90°. `+0x394` (f32) = time since the last record, reset on receive.
2. Behavior record pending → `0xB4E9C0(bsync = modules+0x30, events)` (the same state-diff apply as the ghost) and `0xB4EB70(bsync, rec+0x40, varCount)`. Then the turn target from `+0x148`.
3. Action record pending → copies the two action objects.
4. Moves the chr through **vtable slot 41** (`0x7EB0B0`, move delta) and writes the turn into `manip+0x20`. So the move goes through ChrCtrl/physics (collision kept) and the locomotion blend comes from the same move vector the AI would produce (`0xB4AFD0` reads it) [I].

The behavior sync module exists on enemies: both the host capture (`0xFED3D0` on `bm+0x60`) and the NetAI apply (`0xB4E9C0` on `modules+0x30`, gated only by `bsync+0x10 != 0`) run on EnemyIns [V].

### Recipe (ranked)

**1. Native NetAI drive (recommended).**

Host, per synced enemy, every send tick:
- `netId = u16 enemy+0x78`; `mov = 0xA4B7E0(WCM, netId)`, `beh = 0xA4B740(WCM, netId)`, `act = 0xA1B950(T, netId)`.
- Send the 28 B movement record (20–30 Hz). Flag `0x02` set means slot 11 refreshed it; clear it after reading.
- Send the 344 B behavior record **only when its bytes changed** since the last send. Sending the same record again would replay its `fresh` states.
- Send the 48 B action record when changed.
- Convert target mode 1 (a host netId) to mode 2 with that chr's position, or to 0.

Client:
- Give each host-driven enemy its own copy of the EnemyIns vtable (248 slots plus the locator, as for the ghost). Replace **slot 37** with our function `fn(chr, force: u8)`: for host-driven enemies, call `0xA14B50(chr, 0)`; for all others, pass `force` through. Keep `u8 chr+0x2040 = 0`.
- On receive, write the records into the client's own entry and set the flag bits: movement → `|= 0x01`, behavior → `|= 0x04`, action → `|= 0x40`.
- Write on the game thread, before the chr update (our FrameBegin / UpdateCoords hook).
- Stop forcing the position for these enemies through the UpdateCoords table. The NetAIManipulator moves them itself; keep only a teleport for errors of more than about 8 m.
- To hand an enemy back to the local AI (host gone, own deathblow), simply stop answering `0` in slot 37.

**2. Fallback: freeze AI, keep our own mirroring.**
- `u8 *(chr+0x50) + 0x192 = 1`: ChrCtrl "CompletelyNoMove" (what Lua `SetCompletelyNoMove(entityId, bool)` → `0x68D6D0` → `0x67C850` writes) [V write / I effect].
- The AI still thinks and can still attack.

No WorldChrManDbg "all no think" global was found in this pass (WorldChrManDbgImp is debug-menu only). `chr+0x1F40 >= 0x80` forces MoveSpeedLevel to 0 (earlier note), which is not an AI switch.

### Must be checked live

1. `u16 enemy+0x78 != 0xFFFF` and `enemy+0x2008 != 0` for normal map enemies (the NetAI exists).
2. On the host, the movement flag `0x02` of an enemy's entry is set every frame (ComManipulator slot 11 runs offline). If not, fill the records ourselves with the same layout: physics pos/rot, `0xFED3D0`, `0xFED410`/`0xFED3E0`/`0xFF0630`, `0xFEC1D0`.
3. After the slot-37 swap on the client: `enemy+0x58 == enemy+0x2008` stays true for several seconds, and the enemy stands still without records.
4. Damage both ways while the client enemy is on NetAI. Expected [I]: hits are computed locally offline (no session), so the client's hits lower the local HP (our cumulative-damage path still applies), and the enemy's attack animations run their TAE hitboxes locally and hurt the client.
5. Deathblow / throw on a NetAI enemy: `0xB4E9C0` refuses to apply while the enemy is in a throw state (modules `+0x88`). If the deathblow breaks, return the enemy to the local AI for its duration.

---

## § Event flag storage (2026-10-22, pass 3)

### Layout of SprjEventFlagMan (`self = *(base+0x3D55FE8)`) [V]

| Offset | Content |
|---|---|
| `+0x00`, `+0x08` | raw storage buffers of bank 0 and bank 1 (all bit arrays live in them; `0x6C3DC0` clears both whole) |
| `+0x10`, `+0x18` | their sizes in bytes |
| `+0x28 + 0xF8*bank + 0x18*g` | group descriptor `g` = `id/10^7 % 10` (0..9): `+0x00` ptr to the block-entry array, `+0x08` u32 block-entry count |
| `+0x218` | active bank (pointer to the `+0x28` or `+0x120` descriptors) |
| `+0x220` | bank index (1 = Remnant), `+0x228` u8 ready |

- Block entry = 168 B = **10 records × 16 B** (+8). Record `k` = `id/1000 % 10`. Each record is `{u8* bits, u64 size}`. The size is used as the memset length; [I] 128 for 1000 flags. Bit = `id % 1000`, MSB-first per dword (§1).
- Block index: 0 = global (area 0, block 0). Otherwise `1 + index` of (area = `id/10^5 % 100`, block = `id/10^4 % 10`) in the world block list (`0x82B210(FieldArea+0x18, area, block)` when FieldArea exists, else the WorldMsbList table at `0x3D55FD8`, ≤32 pairs). The list is static map data, so the indices match between machines [I, high].
- Resolver: **`0x6C63F0(self, int id, bool a3) -> record*`**, null if the group or block is unknown. It uses the active bank. Pass 0 for `a3` like SetEventFlag does.
- **A record is exactly one `k` digit of one (group, area, block)**, so `should_sync` (which looks only at the group and `k`) classifies whole records. A record is either entirely shared or entirely local.

### Dump and apply recipe [I, high]

- **Host** (bank 0, `self+0x220 == 0`): for each shared (group, k) (g0: k ∈ {0,1,5,9}, global block only; g1/g2: k ∈ {0,1,4,5,6,7}; g6: k = 0; g7: all k) and each (area 0..99, block 0..9):
  - `rec = 0x6C63F0(self, base, 0)` with `base = g*10^7 + area*10^5 + block*10^4 + k*1000`. Skip null and duplicate records.
  - Send `{u32 base, u16 size, bytes}`.
  - It is about 9000 resolver calls once per join; spread them over frames if needed.
- **Client**, before the join warp (flags live in memory and survive a map reload):
  - Resolve the same bases on its side.
  - **Zero all of its own shared records first**, then copy the host's bytes in.
  - Without the clearing step, a flag the client has set and the host has not (the client's "Chained Ogre killed") survives, and EMEVD disables the Ogre on load. That is the bug that was observed.
  - Write the bytes directly (no SetEventFlag), so no event side effects fire before the reload; the map scripts read the state on load.
  - Keep the existing change-sync afterwards.
- **Size:** at most about 33 blocks (32 + global) × (6 + 6 + 1 + 10 records in groups 1/2/6/7) + 4 global ≈ 763 records × 128 B ≈ **98 KB raw upper bound**. Most records are all zero, so send only non-zero ones (a zeroed record needs only its base) and compress. Expect a few KB to about 20 KB. [G until measured: log the count and bytes on the first dump]

### Are miniboss/boss deaths only flags? [I]

- Bosses and minibosses that stay dead are kept dead by EMEVD on map load, reading map event flags (`1AABBkxxx`, mostly `k` 0/5 → shared).
- Ordinary enemies are not stored at all: they respawn on reload/rest.
- No other persistent "enemy dead" store was found in the flag manager.
- The exact Chained Ogre flag id is EMEVD data and was not determined statically. Live: diff the client's and host's records for area 11 after the kill.

### Confidence summary

| Claim | Level |
|---|---|
| Two contiguous bank buffers; descriptors at `+0x28 + 0xF8*bank`; 10 groups × {ptr, count} | V |
| 168-B block entry = 10 × `{bits*, size}` records, record = `k` digit | V |
| Resolver `0x6C63F0(self, id, a3)` | V |
| Block indices identical on both machines | I (high) |
| Record size 128 B; total ≤ ~98 KB | I / G |
| Miniboss death persisted only through flags + EMEVD | I |

---

## § Native remote player (2026-10-22, pass 4)

Question: is the DS3 "remote player" path (a PlayerIns driven by a NetworkManipulator from received packets, drawn as another player) still in 1.06 and usable offline?
Scripts: `re/re6_job.py` (job runner, adds an `rtti` scan; job lists in `F:\SekiroRE\scripts6\job.txt`) and `re/re6_vtab.py`, run on a private copy `F:\SekiroRE\re6db\quick_re6.i64`. Output: `F:\SekiroRE\out100` … `out112` (`out100/netnames.txt` = all net-related RTTI classes with vtables). Static analysis only. Tags as above.

### Short answer

**Yes, complete, and the developers left an offline test of it in the exe.** The debug step `TestNetStep` has a menu button `"Create LocalRemotePlayer"` that creates a PlayerIns with a **NetworkManipulator** in the player chr set, without any session. The remote player reads its per-frame packet through a small **PlayerSession** interface owned by the chr. Offline that interface is a `PlayerDebugSession`, a loopback into the singleton `NetLocalDebugBufferQueue` that the local player's PadManipulator writes into every few frames. [V for every function below unless tagged]

So the engine already has a seam that is exactly "one byte stream per remote player". Replacing the session object of two chrs (ours and theirs) with our own 12-slot vtable gives the engine's own co-op pipeline over our UDP socket: position/rotation with interpolation, hkb sync states and variables (walk/run blend, attacks), HP and death. No Steam session is involved. [I, high]

### Classes (RTTI, vtable RVAs) [V]

| Class | vtable | Notes |
|---|---|---|
| `ChrManipulator` | `0x29D2AE0` (46 slots) | base. Slot 22 = `manipulator_type()` |
| `PadManipulator` | `0x29D4B30` | local human; ctor `0x7ED3A0`; **sender** (slot 11 `0x7EEB50`) |
| **`NetworkManipulator`** | **`0x29D4530`** | remote player; ctor `0x7EBAB0(mem, holder)`; slot 22 returns **2**; slot 9 = update `0x7EC250`; slot 41 = move `0x7ECF70` |
| `ReplayManipulator` | `0x29D5AE8` | ghost (type 3) |
| `NetAIManipulator` | `0x29D3EB8` | remote enemy (type 5) |
| `ComManipulator` | `0x29D2FB8` | AI |
| `PlayerIns` | `0x2A2B338` (253 slots, locator at `0x2A2B330`) | ctor `0xA217A0(mem, chrInit, playerInit)`; 8848 B |
| `ReplayGhostIns` | `0x2A2D190` | our current ghost |
| `PlayerSession` | `0x2B85FE0` (12) | abstract interface |
| **`PlayerDebugSession`** | **`0x2B86048`** | 8-byte object (vptr only), loopback |
| `PlayerNetworkSession` | `0x2B86150` | 64 B, holds a DLNRD session-member id at `+8` |
| `PlayerNullSession` | `0x2B86308` | static instance `0x3D938D0` (getter `0x1061F20`) |
| `PlayerSessionHolder` | `0x2B862F8` | embedded in PlayerIns at `+0x2020` |
| `NetLocalDebugBufferQueue` | singleton ptr `*(base+0x3D93618)` | non-null in the dump |
| `FrpgSessionManagerImp` | `0x29F9E88`, singleton `*(base+0x3D67C90)` | per-member packet send/recv |
| `SprjSessionManagerImp` | `0x2B8ADB8`, singleton `*(base+0x3D948E8)` | wraps the DLNRD session (`+0x18`) |
| `DLNRD::SessionManager`, `SessionLight`/`SteamSessionLight`, `SessionMemberLight`/`SteamSessionMemberLight`, `SteamSurveillance` (P2PSessionRequest/ConnectFail callbacks), `SteamMatchingSystem` | `0x2E1FB40`, `0x2E26DC8`/`0x2E32980`, `0x2E2BE50`/`0x2E358B8`, `0x2E1CE28`, `0x2E2D680` | Steam transport |
| packet classes | `CSPacketBase` `0x29F8C38`, `CSPacketInheritList16` `0x29F8CA0`, `tag_DP_REGULAR_REPLAY_GHOST` `0x29F8F80`, `CSRegularPacket<DP_PLAYER_ALWAYS_SYNC / _SYNC_VAL_UPDATE / _ALWAYS_SYNC_ON_MOVE_OBJ>`, `CSRegularSyncProperty<DP_GHOST_*>` | the serialised player frame |
| debug steps | `TestNetStep` `0x2A40F30`, `ModuleNetworkHostTestStep` `0x2A64DA0`, `ModuleNetworkClientTestStep` `0x2A64238`, `ModuleCharaManipulatorTestStep` `0x2A5D138` | not instantiated in retail [I] |

There is **no** `NetPlayerIns` or `NetPlayerMemoryManager` class in this exe (no RTTI, no string). Remote players are plain `PlayerIns`; "NetPlayerMemoryManager" in the Sekiro Online note is that mod's own name. The other strings found: `"NetPlayer"` (`0x7B04E0`), `"Remote Player[%d] : Invincible"` (FrpgSessionManager debug draw `0x8E2440`), `IsWhiteGhost_NetPlayer` / `IsBlackGhost_NetPlayer` (ESD/Lua bindings `0x914A0`/`0x91430`).

Manipulator factory **`0x7F8480(playerInit*, holder*)`** switches on `playerInit+4`: 0 base, **1 Pad**, **2 Network**, 3 Replay, 5 NetAI (sizes 832/1104/1056/21552/1104 B; heap `*(base+0x3D9CE18)`). PlayerIns ctor stores the result at `chr+0x58`. Type 6 is the EnemyIns-like Com+NetAI pair.

### The debug "Create LocalRemotePlayer" path [V]

`TestNetStep` menu builder `0xA79840` registers two buttons on callback **`0xA796B0(step, mode)`**: mode 0 = "Create LocalRemotePlayer", mode 1 = "Create Multi Player Memory Check Chr". It also exposes "PacketLossRate" (`step+236`) and "DelayFrameNum" (`step+232`), which the step update `0xA7A080` copies to the loopback queue (`+0x410` loss, `+0x418` delay clamped 1..31).

Mode 1 (separate PlayerGameData; the one to copy):
1. `idx = 0x7B2230()`: first free of 5 remote PlayerGameData slots (in-use bytes at `*(GameDataMan+0x20)`; returns −1 if none). `0x7B4450(idx)` marks it used. `pgd = 0x7B2980(idx)` = `*(GameDataMan+0x10) + 0xA10*idx` (**5 remote PGDs of 2576 B**; the local one is `*(GameDataMan+0x08)`). `0x7B1F80(idx)` frees a slot.
2. Fill the PGD from a CharaInitParam row: `0x1410B8240(tmp, id)` (param #21 lookup; the step uses ids 1080100…1080104 from the table at `0x2A41340`), then `0x7B7470(GameDataMan, pgd, tmp, 0)` and `0x7B7120(GameDataMan, pgd, tmp)`.
3. `u32 pgd+0x10 = playerNo` (the step uses 1..5). **playerNo must differ from the local PGD's `+0x10`**: the lookup `0x7B2C80(no)` checks the local PGD first.
4. `0x7F86F0(&init, playerNo)` builds the 40-byte player init: `{u32 playerNo; u32 manipType = 2; PlayerGameData* = 0x7B2C80(playerNo); u64 sessionMember = 0; void* = MenuMan+0x3238 (0x8C50B0); u32 0}`.
5. **`0xA7A840(step, &init, &chrType, warp)`** (chrType = 1 from `0xA7A830`):
   - `chr = 0xA49460(WorldChrMan, chrType, &init)` (rcx, edx, r8). That is `0xA07960(WCM+0x40, type, init)`: take the first free entry with index ≥ 1 in the **player chr set** (`WCM+0x40` = i32 capacity, `+0x48` = 56-byte entries; entry 0 is the local player; returns 0 if full), build the handle with `0xC33180`, then `0xA05A80` allocates 8848 B, runs the PlayerIns ctor, `0xA065A0`, `0xA2BD90` and vtable slot 14. Afterwards `0xA49460` calls vtable slot 221 (`+0x6E8`) with `L"WorldChrMan"`.
   - `0x9F5170(chr, chrType)` (type setter, recomputes team), then vtable slot 18 (`+0x90`, `0xA23790`).
   - For types 1, 2 and 12, it clears bit 0x08 of `u8 chr+0x1A11`. With `warp`, it sets the bit again and calls `0x9F6320(chr)`, which places the chr next to the main player.
   - `0x89BB20` (a "register net player" call) is stubbed to `return 0` in retail.

Mode 0 skips steps 1–3 and passes the local player's own number, so the clone **shares the local PGD** (same equipment/face). Do not use that for real partners: the NetworkManipulator writes HP/max HP into its PGD every packet.

The PlayerIns ctor [V]:
- `chr+0x2000` = PGD (`init+8`). `chr+0x2020` = PlayerSessionHolder (ctor `0x1061D90`: `+0x08` debug session owned, `+0x10` network session owned, **`+0x18` = active session** (so **`chr+0x2038`**), `+0x20` i32 = −1).
- If `init+0x10 != 0`, it calls `0x1062080(holder, member)`: a PlayerNetworkSession for that member. If it is 0, it calls **`0x1062010(holder)`: a new PlayerDebugSession** (whose ctor increments the queue's subscriber count `+0x420`).
- **The main player is created the same way with `init+0x10 = 0`** (`0x7F8730`, from `0xA058A0`). So offline the local player also owns a PlayerDebugSession. Only a real session start (`0x8E4DE0`, `0x8E74D0`) swaps it for a PlayerNetworkSession with the own member id.

### Data flow: sender → session → receiver [V]

**Sender: PadManipulator slot 11 `0x7EEB50(pad, ctrl)`.** Every `0x96F8A0(5)` frames, or when the forced byte `chr+0x2201` is set:
1. `0xB4F1A0(bsync, force, tx)` fills the sync states and variables from the recorder (`0xFED3D0`, `0xFECE80`, `0xFEC790`; same as the replay recorder).
2. It writes into `tx = *(pad+0x350)`: position (`0x8DC660`), rotation (`0x8DBA40`), HP and max HP as u16 (`0x8DC3C0`/`0x8DC510`, from the data module), the lock/target handle (`tx+0x2800`, from PadManipulator slot 21), the move-object relative position (`0x8DBFE0`, only when standing on a moving object), and more.
3. `0x8DA080(tx)` serialises with **`0x8D7000(tx, BitWriter*, 1024)`** (the same serialiser as replay frames) and calls **`session->vtbl[1](session, bytes, size, type = 1)`** on `*(*(tx+0x1E0)+0x18)`, i.e. the holder's active session.

**Receiver: NetworkManipulator.** `+0x340` = rx packet (10688 B, ctor `0x8D46C0(mem, holder)`, base `0x8D3C60`); `+0x348` = a second packet (9896 B, `0x8D4260`; used only when config byte `0x3B066E6` is set).

Packet layout (base `0x8D3C60`):
- `+0x000` DP_REGULAR_NORMAL, `+0x098` DP_REGULAR, `+0x138` DP_REGULAR_REPLAY_GHOST.
- `+0x1E0` holder*.
- `+0x1E8` sync-event vector (`NetPackingVector<BehaviorStringIdSyncInfo>`).
- `+0x210` `HkBehAllVariable`.
- `+0x27E0`, `+0x27E1`, `+0x1D8`, `+0x27F0` bool flags.
- `+0x27E4` vec3, `+0x2800` FIELD_INS_HANDLE.
- `+0x2808` packet list (CSPacketInheritList16).
- The receive accessors read pos `+0x2964`, rot `0x8D6210`, HP `u16 +0x2980` (`0x8D6290`), max HP `0x8D62D0`.

NetworkManipulator update `0x7EC250` (slot 9):
1. `while (0x8D8050(rx))`. `0x8D8050` calls **`session->vtbl[6](session, &out, &buf{ptr, cap = 1024}, type = 1)`**. If it gets bytes, it parses them (`0x8DD210`, list deserialise through `rx+0x2808` vtbl[1], `0x8E8510` ×2, `0x8DEA40`), then calls session vtbl[11] and validates the member (`0x141BD9510`; with an empty member it calls `0x8DD290`).
2. For each packet it calls:
   - `0x7EC9E0`: target position `manip+0x360` and rotation `+0x370`, velocity = delta / (1.2 × time between packets), snapping if the speed is over 20 m/s or the interval is not in (0, 2] s. This is the same scheme as the NetAI manipulator.
   - **HP** → `0xBD64E0(dataModule, hp)`. If HP drops from > 0 to 0, it kills the chr with `0x9EE7E0(chr, 1)`. Max HP → `0xBD6550`. It also writes PGD `+0x18`/`+0x1C`/`+0x5E8`.
   - The look byte (`chr+0x1A30` → `0x9F6780`).
   - **`0xB4EFE0(bsync, rx)`**: `0xFFEBB0(hkChr, rx+0x210)` applies the variables if `u8 bsync+0x1A`; `0xB4E9C0(bsync, rx+0x1F0, rx+0x27E0)` applies the sync-state diff if `u8 bsync+0x19`. These are the same applies the replay ghost uses.
3. Moves through slot 41 `0x7ECF70`. It classifies the move speed ×30 into levels 0/1/2/3 (≥ 0.01, ≥ 2, ≥ 6 m/s, with a 0.2 s hysteresis at `+0x3D0`/`+0x3D4`) and writes the move vector `manip+0x10` that the locomotion graph reads. **Walk/run blend and attacks therefore come from the engine.**
4. Remote-side checks use `0x9F36C0(chr)`: `vtbl[53]() && chr != main player && !vtbl[54]()`. PlayerIns slot 54 `0xA2B4E0` is true only for `PGD+0x10` in 10000000..10000039 (NPC phantoms). A PGD with playerNo 1..5 is "remote". Remote chrs skip the local locomotion writer `0xB4AFD0`, the stat recompute (`0xBD5EA0`/`0xBD5FF0`/`0xBD60F0`) and SpEffect HP damage (`0xB658B0`).

**Sessions** (slot: Debug / Network) [V]:
- 1 `send(this, u8* data, u32 size, u32 type)`: `0x1061810` → `0x105E580` (queue push) / `0x1061CE0` → `0x8E7210` (broadcast).
- 2..5 typed sends: types 20 (192 B), 23, 37, and a 4-byte one.
- **6 `recv(this, Out* out, Buf* buf, u32 type) -> Out*`**: `0x1061700` → `0x105E420` / `0x1061B40` → `0x8E6B50(mgr, member, type, ptr, cap)`. `Out` is 24 B, `{u8* ptr; u32 cap; u32 pad; u64 size}`, all zero when nothing is pending.
- 7/8: recv of types 60/61 through slot 6.
- 9/10: misc (Network slot 10 sends type 39).
- 11 `get_member(this, out*)` (Debug returns an empty member).
- 0: dtor.

**Loopback queue** (`*(base+0x3D93618)`, ctor `0x105DF70`) [V]:
- 32 frame slots of vectors of 2056-B entries `{u32 size; u32 type; u8[2048]}`.
- `+0x414` write index (advanced and cleared by `0x105E810`, called from the session-manager update `0x8E77E0`); `+0x418` delay in frames (default 1); `+0x410` loss rate (default `*(0x3D93610)`); `+0x41C` u8 "drop this frame"; `+0x420` subscriber count.
- Push = one copy per subscriber into the current slot. Recv = the first entry with a matching type in slot `write − delay`, removed.

### Recipe (ranked)

**N0. One-machine smoke test (no new code paths).** On the game thread, after the map has loaded:
1. Run mode 1 of `0xA796B0` yourself: steps 1–4 above, then `0xA49460(WCM, 1, &init)`, `0x9F5170(chr, 1)`, `chr.vtbl[18](chr)`, `0x9F6320(chr)`.
2. The new chr gets a PlayerDebugSession, so it should copy the local player's moves one frame late (delay = 1).

This checks spawn, the model (it appears with the template's equipment), packet parsing, sync-state apply and the locomotion blend, before any networking. [I, high]

**N1. Native remote player over our socket (recommended).**
- **Spawn**: as N0, with our playerNo. Fill the PGD from the partner's data (see Appearance).
- **Receive**: give the remote its own session object: an 8-byte block whose vptr points to our copy of the PlayerDebugSession vtable (copy 12 qwords from `0x2B86048`). In the copy, slot 6 is our `recv` (pop one packet of `type` from our per-type queue for this partner, copy at most `cap` bytes to `buf.ptr`, fill `Out`), and slot 1 is a no-op. Write its address to **`chr+0x2038`**. Leave `holder+0x08` alone: its debug session is still freed by the holder dtor `0x10620F0`, which then resets `+0x18` to the null session, so our object is never freed by the game. Write on the game thread before the chr update (FrameBegin).
- **Send**: replace the local player's `*(player+0x2038)` with our session copy whose slot 1 forwards `(type, bytes)` to UDP and then calls the original `0x1061810`, so local behaviour stays the same. Type 1 is ≤ 1024 B about 12 times a second at 60 fps (**≤ 12 KB/s**). Forward the other types the slot sees too, and log which ones appear.
- **Appearance**:
  - The parts come from the remote PGD. Either copy the partner's equipment ids and face into the PGD before spawning, or after spawning use the existing `dress_like` (`0x9CCBA0` on `chr+0x2140`) and then `chr.vtbl[21](chr)` / `0xA361C0` to reload the parts.
  - Chr type 1 is white phantom: the param-72 row keyed by chr type probably gives phantom shading and the team. Use the "Solid ghost (2)" slot-130 override on a per-object PlayerIns vtable clone (`0x2A2B338`, 253 slots, locator `0x2A2B330`). This time slot 248 is already false, so the draw group is 7 ("other player").
- **Despawn**: as for other chrs. Also free the PGD slot with `0x7B1F80(idx)`.

**N2. Feed the native Steam receive path (not recommended).** `0x8E6B50` returns nothing unless `0x1072FE0()` says the SprjSessionManager is in a session and `0x8E4170(mgr, member)` finds a member record. Faking that means fabricating DLNRD `SessionLight`/`SessionMemberLight` state with Steam ids. It would also switch on every online system (summon/sign state, EMEVD multiplayer conditions, penalties, Steam P2P calls through `SteamSurveillance`, the member-count gate `(*(S+0x168) − *(S+0x160))/56` that also flips the enemy NetAI logic). This is weeks of work with a high risk of crashes or bans, and it gives nothing N1 lacks for players. [I]

### Hit, damage, lock-on [V routing, I meaning]

- **Damage to the remote**: `0xB729E0` routes damage on a remote chr to `0xB68FF0(…, remote = 1)` instead of the normal `0xB6A040`. HP is owned by the sender's packet, and SpEffect damage and stat recompute are skipped. Expect enemy hits to play hit reactions on the remote without lowering its HP locally. The partner's real HP drops on their own machine, where the enemies are synced too. [I]
- **Local player ↔ remote**: `0xBE3DA0` applies a separate correction (`0x8B3540(65, …)`) when one side is local and the other remote, i.e. a PvP-style path exists. Friendly-fire depends on the team type, which `0x9F64D0` takes from the param-72 row keyed by chr type (type 1 → expected "white phantom" team) [G until read live].
- **Remote's attacks hitting enemies locally**: not resolved statically. Natively, the attacker's machine evaluates its hits and sends damage packets, so the local copy may deal no damage [G]. Keep our host-authority cumulative damage (netai) either way.
- **Lock-on and nameplate**: `0xA4E8E0` excludes remote chrs from one candidate list. No nameplate path works without session player names [G]. Neither is needed for co-op.

### Must be checked live

1. `i32 WCM+0x40` (player-set capacity, expected 6) and the local `PGD+0x10` value. Choose playerNo ≠ that.
2. The RTTI of `*(player+0x2038)` offline: expected `PlayerDebugSession`.
3. N0: the clone appears, follows the player with a 1-frame delay, walks and runs with the blend, and repeats attacks. `*(clone+0x58)` should be a NetworkManipulator (`vtbl[22]()` returns 2).
4. PadManipulator slot 11 runs offline: count the slot-1 calls in the send tee (about 12 per second).
5. `u8 bsync+0x19` / `+0x1A` on the remote (`bsync = *(chr+0x1FF8)+0x30`) are 1 (event/var apply enabled).
6. Team `u8 chr+0x74` for types 1 and 8. Whether enemies target the remote. Whether the remote's TAE hitboxes damage enemies or the local player.
7. Cross-machine fields: the handle at `rx+0x2800` (lock target) and the move-object handle are machine-local. Watch for wrong facing on lifts and lock-on. Also check that the remote survives a map reload (otherwise respawn after every load).
8. The phantom look with type 1. Apply the slot-130 override and compare with `player.vtbl[130]`.

### Effort vs. the current ghost approach [G]

- **N1**: about 1–2 days for spawn + sessions + the N0 smoke test, plus 2–4 days of live work on appearance and the hit/team questions.
- It replaces most of `ghost.rs`/`motion.rs`: the UpdateCoords pinning, our `manip+0x10` writes, and the hkb event and variable mirroring. The engine then does interpolation, locomotion blend, attacks, HP and death from one ≤ 1 KB packet.
- Risk is moderate. All entry points are retail code that the dev test path calls. The unknowns are game-thread timing, the debug-session subscriber count, and per-machine handles in the packet.
- The ghost approach stays the fallback.

### Sekiro Online note (public info + exe only)

The closed mod's binaries were not touched. The exe has the full DS3 transport: `FrpgSessionManagerImp` per-member typed packets (send `0x8E6FA0`, broadcast `0x8E7210`, recv `0x8E6B50`) over `SprjSessionManagerImp` → DLNRD `SteamSessionLight` (Steam lobby + P2P). "Uses the game's own session" therefore most likely means running this stack on real Steam lobbies [G]. For our offline mod, the PlayerSession seam above sits one level higher and avoids all of it.

### Confidence summary

| Claim | Level |
|---|---|
| PlayerIns + NetworkManipulator (type 2) creation via `0xA49460` / `0xA07960` without a session, as used by the debug step | V |
| PGD slots (5 × 0xA10 at `GameDataMan+0x10`) and the init struct layout | V |
| Holder at `chr+0x2020`, active session `chr+0x2038`; `init+0x10 == 0` → PlayerDebugSession (also for the main player) | V |
| Session vtable slot meanings (1 send, 6 recv, 11 member) and the `Out` layout | V (code) / I (names) |
| Type-1 packet = pos/rot/HP/sync states/variables, ≤ 1024 B, serialised by `0x8D7000` | V |
| NetworkManipulator applies it through `0xB4EFE0` / `0x7EC9E0` / slot 41 (locomotion blend native) | V |
| Swapping `chr+0x2038` for our own vtable object is enough to drive a remote player from UDP | I (high) |
| Remote damage routing (`0xB729E0`), HP owned by the packet | V (routing) / I (effect) |
| Team type, enemy targeting, the remote's hitboxes, phantom look with type 1 | G |
