# Sekiro Seamless Co-op: feasibility study and development plan

Date: 2026-10-22 (rev. 2: development-ready plan, compatibility findings, setup). Method: read-only research. Nothing in either game folder was touched, the game was not run, and no crack was downloaded or executed. Three source repos plus me3 were shallow-cloned into a scratch folder to be read (not built or run). The user's existing `Downloads\SSDT_Fix_Repair_Steam` pack was inspected only by reading PE headers and the `.ini` (nothing executed).

## Lyhyesti suomeksi

- Avoimen lähdekoodin pohjaa on, mutta kukaan ei ole vielä saanut **toista pelaajaa näkymään Sekiron maailmassa**. Se on koko projektin vaikein kohta, ja siksi ensimmäinen kehitysvaihe testaa juuri sitä.
- **Miksi modit eivät toimineet alkuperäisellä exellä:** todennäköisesti syy ei ole Arxan. me3:n kehittäjien mukaan Sekiro on ainoa FromSoftin PC-peli ilman Arxania. Teidän "fix"-pakettinne käyttää CreamAPI:a, joka esittää pelin Steamille Spacewarina (AppID 480). Ongelma oli siis luultavasti Steamin verkkoyhteys ("Waiting for Steam"). Lisäksi kaksi `dinput8.dll`-modia ei mahdu samaan kansioon.
- **Meidän modimme ei tarvitse crackia eikä Steamin verkkoa.** Se käynnistyy me3:lla alkuperäisestä Steam v1.06 -exestä ja käyttää omaa UDP-yhteyttä (esim. Tailscalen yli). Sessio on meidän omamme, joten kuolema, warppi tai bossi ei katkaise sitä. Tämä on juuri seamless-idea.
- **Suositus:** kehitys aloitetaan, mutta vaiheittain. Vaihe 1 (2–4 viikonloppua) on go/no-go-testi: saadaanko toinen hahmo maailmaan ja liikkumaan. Jos ei, lopetetaan siihen.

---

## 1. `vswarte/fromsoftware-rs`, crate `sekiro`

Repo: https://github.com/vswarte/fromsoftware-rs (HEAD `6da5f80`, 2026-10-21, very active). Crate: https://github.com/vswarte/fromsoftware-rs/tree/main/crates/sekiro, docs at https://docs.rs/sekiro.

**Game version targeted.** `crates/sekiro/src/rva.rs` accepts only PE file version `1.6.0.0` (EN and JP) and **panics on anything else**. That is Steam v1.06, the final patch. RVAs are generated from `mapper-profile.toml` via `tools/binary-mapper`.

**What it covers** (`crates/sekiro/src/`, about 3.5k lines in `sprj/`):

| Area | Files | Useful for co-op? |
|---|---|---|
| Task system: `SprjTaskImp::wait_for_instance`, recurring per-frame tasks | `sprj/task.rs` | **Yes.** It gives a safe per-frame hook on the game thread |
| Event flags: `SprjEventFlagMan::set_flag/get_flag`, flag zones | `sprj/event_flag.rs` | **Yes.** World-state sync and EMEVD triggers |
| Params: `SoloParamRepository`, generated param structs | `sprj/solo_param_repository.rs`, `param/generated.rs` | Yes: NpcParam, ChrInitParam, scaling |
| `GameDataMan`, `PlayerGameData` (stats, inventory) | `sprj/game_data_man.rs`, `player_game_data.rs` | Yes: save/progress sync |
| Items: `MapItemMan::grant_item`, `LuaEventMan::remove_item` | `sprj/item.rs`, `item_id.rs` | Item sync |
| `FieldArea`, `WorldInfo`/`WorldRes` (area and block tables) | `sprj/field_area.rs`, `world_info.rs` | Which map or block each player is in |
| Menus (`MenuMan`, `NewMenuSystem`), allocators, file I/O, STL | `app_menu`, `dlkr`, `dlio`, `stl.rs` | UI later |

**What it does NOT cover:** `ChrIns`, `PlayerIns`, `WorldChrMan`, character spawning, animation (TimeAct), physics or Havok position, networking or sessions, warps. The Elden Ring crate has all of these, including `WorldChrMan::spawn_debug_character` (`crates/eldenring/src/cs/world_chr_man.rs:138`), which works by filling `CSDebugChrCreator.init_data` and setting `spawn = true`. The DS3 crate (Sekiro's engine ancestor) has `ChrIns`, `chr_module` and a `WorldChrMan` with `ChrSet` iteration, but no debug spawner. **Those DS3 and ER structures are the best map for the Sekiro work that has to be reverse-engineered.**

**How mods load.** Write a Rust `cdylib` with an exported `DllMain`. It spawns a thread, calls `SprjTaskImp::wait_for_instance`, then registers recurring tasks; the pattern is in `examples/debug-line/src/lib.rs` (ER, but the Sekiro task API is the same shape). The DLL is loaded by **me3** as a `[[natives]]` entry in a `.me3` profile with `[[supports]] game = "sekiro"` (see §5). There is also an ImGui debug overlay for Sekiro in `tools/debug-sekiro`.

**License caveat.** `Cargo.toml` says `MIT OR Apache-2.0`, but the actual files are `LICENSE-MIT-NON-AI` and `LICENSE-ASL2-NON-AI`. These are custom variants that forbid using the code to *train or improve* ML models or putting it in training datasets. So they are not OSI open-source licenses. Building a mod on the crate is fine. Writing our code with an AI assistant is not training, so I read it as allowed, but the wording is broad: ask the maintainer (Discord `@chainfailure`, per README) for a one-line confirmation. **See §8.1 for why this clause matters if we mix in AGPL code.**

## 2. `mstampfli/sekiro-coop`

Repo: https://github.com/mstampfli/sekiro-coop. **6 commits, 2026-04-19 to 2026-06-15**, author Marc Stampfli. Nothing since June, so treat it as dormant. Handoff doc: https://github.com/mstampfli/sekiro-coop/blob/main/docs/HANDOFF.md (itself written for an AI coding agent).

**Targets:** Sekiro v1.06 (`70066176`-byte exe, timestamp `0x5fa3e066`), Rust, MinHook detours, loaded via me3 v0.11.0 with `--disable-arxan true --online false`. It does **not** use fromsoftware-rs; it has its own `sekiro-sdk-sys`.

**Validated (against one real Sekiro plus a `peer-simulator` stand-in, never two real games):**
- DLL injection via me3; 6 native hooks: `SetFlag`, `ApplyEffect`, `DeleteEffect`, `GiveItem`, `AddExperience`, `WarpBonfire`.
- UDP P2P session with handshake, sliding-window reliability, desync hash check.
- 60 Hz own-player snapshots; edge-triggered replication of flags, effects, items, XP and warps; remote events are applied by calling the original function through the trampoline, so they don't echo back.
- Enemy registry (42 entities) fed from `ApplyEffect` hook arguments. Blind `WorldChrMan` sweeps crashed, which is consistent with the Cheat Engine crashes the user saw.
- Host-authoritative enemy HP: written through `ChrIns+0x1ff8 → +0x18 → +0x130`, decrement-only.
- An EMEVD reader/writer with a 386-opcode catalog, "SOLO-branch promotion" and custom-event injection (untested against a real game file).

**Blocked:**
- **The remote player is not visible.** Raw writes to the `ChrIns` position are stored but ignored by rendering and physics; the "puppet experiment" failed (HANDOFF §3, §12).
- No native addresses for `RequestAnimationPlayback`, `ForceAnimationPlayback`, `CharacterWarpRequest`, `SetCharacterAiState` and the other natives needed to drive a puppet NPC.
- The planned unblock (UXM unpack, then Yabber DCX, then patch `common.emevd`, then trigger custom events via flags) was **never started**; `CharacterWarpRequest` only warps to predefined points, not arbitrary XYZ.
- No damage-application hook, no death handling, no NAT traversal, no reconnect; the rollback crate is not wired in; `GAPS.md` lists 15 open reverse-engineering items, including `WorldChrMan` itself.

**License: confirmed, no LICENSE file.** The repo has no top-level LICENSE or COPYING file; the only license texts are for vendored me3 (MIT/Apache) and MinHook (BSD-2). **But** the README states "AGPL-3.0-or-later" and the workspace `Cargo.toml:22` has `license = "AGPL-3.0-or-later"`. The intent is declared and unambiguous, so forking is legally workable, as long as the derived mod is also AGPL-3.0. Contacting the author is not strictly required, but recommended: ask them to add the LICENSE text and whether they'd collaborate.

**Bundled binaries:** it checks in `tools/me3/me3-windows-amd64.zip`, me3 `.exe`s and a prebuilt `vendor/lib/MinHook.x64.lib`. These are well-known upstream tools, but **don't run them from this repo**: get me3 from its official releases and build MinHook from source.

## 3. `micthiesen/unseamless-coop` (Elden Ring, MIT)

Repo: https://github.com/micthiesen/unseamless-coop. MIT, HEAD 2026-09-22, about 50 commits, built on fromsoftware-rs. It is a clean-room rewrite of LukeYui's ER Seamless Co-op.

**What transfers:**
- **Workspace split by verifiability** (`docs/ARCHITECTURE.md`): a pure-Rust core (config, state model, protocol) unit-tested on the host, plus a thin `cdylib` that only reads and writes game singletons.
- **Feature-per-task model**: each feature registers its own recurring game task in a chosen frame phase (fromsoftware-rs `SprjTaskImp`).
- **Safety invariants**: never unload on DLL detach, and gate every memory walk on "fully in-world".
- **Rig ideas** (`docs/LOCAL-DUO.md`): probe logs plus auto-screenshots, so "can I see the peer?" is answered from logs. Two clients on one machine needed two Steam accounts and two installs.
- **Clean-room wording**: "reimplemented from observation, contains no upstream code". This is our stance towards LukeYui's mod.

**What does NOT transfer:** they drive ER's live native P2P stack. Sekiro shipped without online play. However, DS3 networking code is still inside `sekiro.exe`: "The vulnerable code is also present in Sekiro (credit: LukeYui)" (https://github.com/tremwil/ds3-nrssr-rce). sekiro-coop's `GAPS.md` also lists 21 network EMEVD instructions. LukeYui's Sekiro Online very likely revives this dormant code, so sekiro-coop's claim of "no phantom player slot" is unproven.

**Sobering data point:** with a fully live native stack, unseamless-coop's `docs/STATE.md` (2026-08-23) reaches `players=2` on two real machines but **zero phantoms spawned** (`join_wait=true`). Making the other character appear is the wall in both games.

## 4. Other references

| Project | What it is | Relevance |
|---|---|---|
| **HKMP** (Hollow Knight), https://github.com/Extremelyd1/HKMP, LGPL-2.1 | Full multiplayer mod: in-game host or standalone server; player-level sync | Architecture reference for the **puppet model**: remote players are local stand-ins driven by network state |
| **SekiroResurrection/modding-wiki**, https://github.com/SekiroResurrection/modding-wiki | Community wiki (characters, formats); notes that Sekiro and DS3 differ in significant and undocumented ways | Lookup for character IDs, EMEVD and param formats |
| **borgCode/SekiroTool**, https://github.com/borgCode/SekiroTool, MIT | Practice tool (likely C#/.NET 4.8): disable AI, noclip, free cam, idol/boss warps, item spawn | **Valuable:** working offsets for AI disable and **player teleport** on Sekiro, the primitives a puppet needs |
| **Yui's Sekiro Debug Menu** (credited by SekiroTool) | Restores FromSoft's debug menu incl. GAME → CHR INS → WorldChrMan | Implies Sekiro still has debug character tooling; ER's equivalent `CSDebugChrCreator` is how fromsoftware-rs spawns characters, so **it's the #1 thing to look for** |
| **Sekiro Mod Engine** (katalash), https://www.nexusmods.com/sekiro/mods/6 | `dinput8.dll` proxy, file redirection, chainloading via `modengine.ini` | Older loader; see §5 for why it often clashed |
| **dearxan**, https://github.com/tremwil/dearxan, MIT/Apache | Runtime Arxan neutering, integrated into me3 as `disable_arxan` | Supports DSR, DS2, DS3, ER, AC6, NR; **Sekiro not listed** (Sekiro has no Arxan, §5) |
| Nexus "Sekiro Modding Basics", https://www.nexusmods.com/sekiro/articles/279 | Offline enemy placement via MSB plus EMEVD | Fallback puppet source: a dormant NPC placed in map files |
| LukeYui Patreon, https://www.patreon.com/LukeYui/posts/update-current-129620429 | Sekiro Online "aged rather poorly"; Seamless rewrite has serverless connections but no user features | Official route stalled, no public release |

**Fake repos:** no legitimate "Sekiro seamless co-op" release exists anywhere. Treat any `.exe` "seamless suite" as malware; none was downloaded. LukeYui now publishes as `yuiamoroll` (ER Seamless, DS2 Seamless alpha).

## 5. Why mods failed on the unmodified install, and how legitimate loaders handle it

### 5.1 Arxan is probably NOT the cause

The me3 maintainers' Arxan reverse-engineering article (in the me3 repo, `docs/blog/posts/arxan-reversing-1.md`) says FromSoftware "has applied it to all of their PC releases since Dark Souls II: SOTFS, **excluding Sekiro**". dearxan's own game list also omits Sekiro. So sekiro-coop's `--disable-arxan true` is very likely a no-op on Sekiro. The guess "Arxan blocked the mods" does not fit the evidence. **Verified 2026-10-22:** me3's log on this machine reports `dearxan::disabler: arxan detected: false` for Sekiro 1.06.

**Found in testing (2026-10-22):** fromsoftware-rs `sekiro` 0.14.0 (and git HEAD) can never detect the exe. Its `NAME` is `"sekiro: shadows die twice"`, but `normalize()` strips `:` and `™` from the ProductName `"Sekiro™: Shadows Die Twice"`. We patch this locally in `vendor/sekiro/src/rva.rs`; it should be reported upstream.

### 5.2 What the "SSDT_Fix_Repair_Steam" pack actually does (read-only inspection)

| File | Finding |
|---|---|
| `sekiro.exe` | PE timestamp `0x5fa3e066` (2020-11-05 11:22 UTC), file version `1.6.0.0`, CompanyName FromSoftware. This is the **same build** as official v1.06 (sekiro-coop records the same timestamp). But it is **68,005,448 bytes vs. 68,005,144** for the Steam-verified official exe (measured 2026-10-22, only 304 bytes larger), and Authenticode **NotSigned**, so it has been lightly modified (likely a small patch, not a rebuild). It still has a `.bind` section (normally the SteamStub DRM marker) and an unusual `.interpr` section. I did not analyze it further. |
| `steam_api64.dll` + `steam_api64.org` + `cream_api.ini` | CreamAPI in **`wrappermode = true` with `newappid = 480`**. That makes Steam see the game as **Spacewar (App ID 480)**, Valve's public test app. `language = russian` suggests a Russian repack origin. |
| `dinput8.dll` | Sekiro Online, patched only to call `steam_api64.org` (per the requesting session's diff) |

**Interpretation (a hypothesis, not verified):** the pack exists to get **Sekiro Online's Steam networking working**. With App ID 814380, the friends-list invites, P2P and summon-sign server lookups evidently did not come up for these users; through Spacewar's App ID they do. This matches the observed symptom: Sekiro Online sitting at **"Waiting for Steam"**. Spoofing App ID 480 is a common community workaround, but it is a ToS grey zone and depends on a cracked DLL and exe, **so we will not use or replicate it.**

### 5.3 Other likely causes on the clean install

- **The crash at `0x1408D9016`** is RVA `0x8D9016`, inside `sekiro.exe` `.text`. Without a crash dump it can't be attributed. Plausible causes: an exe that is not exactly v1.06 (Sekiro Online and fromsoftware-rs both need exactly 1.6.0.0); a second hooker on the same function (the user's Cheat Engine HP hooks crashed together with Sekiro Online); or Sekiro's own engine bugs. me3 documents one: Sekiro "consistently causes OOB reads in a certain `GXFlverMaterial` function" (`crates/mod-host/src/alloc_hooks/mimalloc.rs`), which me3 mitigates by replacing the allocator.
- **`dinput8.dll` collisions.** Sekiro Online, Sekiro Mod Engine and many other Sekiro mods (Weapon Wheel, Hotkey System) all ship as `dinput8.dll`. Only one can sit next to the exe; others must be chainloaded through `modengine.ini`, and the load order itself can crash. Copying one mod over another silently removes the first.
- **Version gating.** Sekiro Mod Engine's Nexus page says it supports Steam v1.02–1.04; a later version added an AOB scanner and an exe-version check that **disables itself** on a mismatch. On v1.06 it may simply do nothing.

### 5.4 How a legitimate loader (me3) avoids all of this

From the me3 source (`garyttierney/me3`, HEAD 2026-10-13):
- **No files in the game folder are replaced.** me3's launcher starts `sekiro.exe` itself with `CREATE_SUSPENDED` (`crates/launcher/src/game.rs`) and injects its mod host before game code runs. No `dinput8.dll` hijack means no collisions, and Steam "Verify integrity" keeps passing.
- It sets `SteamAppId` in the game's environment (`crates/cli/src/commands/launch.rs:508`), so Steam API init works under the **real** App ID 814380. Its troubleshooting guide says "Ensure Steam is running before launching me3".
- **Sekiro-specific stability patches:** allocator replacement (mimalloc, 6 GB pre-committed heap for Sekiro, to absorb the `GXFlverMaterial` OOB reads) and an "oversized regulation fix" (`crates/mod-host/src/savefile.rs`), plus logo skipping.
- Sekiro has first-class support (`sekiro`/`sdt` game ID, Steam App ID 814380 in `schemas/mod-profile.md`) since me3 v0.6.0.
- Mods are listed in a `.me3` profile: `[[natives]]` for DLLs and `[[packages]]` for file overrides, so several DLL mods can coexist with an explicit load order.

### 5.5 What this means for our mod

- **Target: the original Steam v1.06 exe, launched by me3.** No crack, no CreamAPI, no App ID spoofing.
- **Our networking does not use Steam at all** in the first versions: plain UDP between the two PCs over a free mesh VPN (Tailscale or ZeroTier) or a forwarded port. This removes the whole "Waiting for Steam" class of failure. Steam Networking Sockets under the real App ID can be evaluated later, but it is optional.
- **Never install alongside Sekiro Online** in the same launch (two co-op stacks hooking the same functions will crash, as CE did).

## 6. Setup the user needs (both players)

1. **Close the game.** Back up the current game folder (the one Steam uses, `F:\SteamLibrary\steamapps\common\Sekiro`) to e.g. `F:\Backup\Sekiro-SSDT-2026-10-22\`, so Sekiro Online can be restored later. `F:\Games\Sekiro` appears to be a second copy: leave it alone.
2. **Back up saves:** `%APPDATA%\Sekiro\` → a dated zip.
3. **Remove the extra files** from the Steam game folder: `dinput8.dll`, `steam_api64.org`, `cream_api.ini`, `SekiroOnlineFont.ttf`, and any `modengine.ini` or mod folders. Steam's verify **restores modified files but does not delete extra files**, so a leftover `dinput8.dll` would still be loaded.
4. **Steam → Sekiro → Properties → Installed Files → Verify integrity of game files.** This restores the original `sekiro.exe` and `steam_api64.dll`.
5. **Check:** `sekiro.exe` → right-click → Properties → Details shows file version **1.6.0.0**, and the size is **68,005,144 bytes**. That size was measured on this machine right after Steam Verify on 2026-10-22; sekiro-coop's figure of 70,066,176 does not match this install. Launch once through Steam normally to confirm it runs clean.
6. **Install me3** from the official releases only: https://github.com/garyttierney/me3/releases (Windows installer).
7. **Install Tailscale** (free) on both PCs and sign in so both machines see each other. Port forwarding works too, but is fiddlier.
8. The friend needs **their own legitimate Steam copy** of Sekiro, set up the same way. Two copies on one PC is not a supported test path (Steam runs one copy per account); our development loop uses a peer simulator instead (§9).
9. Launch: Steam running, then our me3 profile (the exact command ships in our README with the first build).
10. To go back to Sekiro Online: restore the backup from step 1. Never mix both.

## 7. Technical pieces: what exists and what must be reverse-engineered

Legend: ✅ exists and validated · 🟡 exists partly or unvalidated · ❌ must be reverse-engineered.

| Piece | Status | Where / what's missing |
|---|---|---|
| DLL loading on v1.06 | ✅ | me3 + fromsoftware-rs `SprjTaskImp` |
| Per-frame game-thread tick | ✅ | fromsoftware-rs recurring tasks |
| P2P transport, reliability, desync check | ✅ (design) | sekiro-coop's net crate shows the design; we re-implement it (license, §8.1) |
| Event-flag read/write | ✅ | fromsoftware-rs `SprjEventFlagMan` |
| Item/XP/effect replication | 🟡 | sekiro-coop proved the hook approach against the simulator |
| Local player position/animation read | 🟡 | sekiro-coop chains; Cheat Engine tables; SekiroTool |
| `WorldChrMan` / `ChrIns` layout for Sekiro | ❌ | Port from fromsoftware-rs DS3 structs, validate on v1.06 |
| **Puppet: spawn a character** | ❌ **(hardest)** | (a) Sekiro debug chr creator (ER analog known); (b) dormant NPC via MSB/EMEVD; (c) dormant DS3 phantom code |
| **Puppet: move visibly** | ❌ | Raw `ChrIns` position is ignored; need the physics/Havok proxy position or native warp (SekiroTool's teleport is a reference) |
| **Puppet: play animations** | ❌ | TimeAct/animation module or the native behind `ForceAnimationPlayback` |
| Puppet AI off, team/hostility | 🟡 | AI disable in SekiroTool; team enums in sekiro-coop |
| Enemy authority/sync | 🟡 | HP write works; **posture**, deflect/stagger, AI targeting of the puppet and deathblows are missing |
| Guest damage to enemies | ❌ | No damage-application AOB; guest hits are forwarded to the host |
| Boss fights | ❌ | Per-boss EMEVD: fog gates, phases, cutscenes, deathblow markers, kill flags |
| Death/respawn without ending the session | ❌ (game side) / ✅ (session side) | Our own session never ends on death (§8.2); the game side needs puppet despawn/respawn and world-reset handling |
| Loading screens/warps | ❌ | Puppet is re-created after every load; players in different areas are handled |
| World/progress flags | 🟡 | Flag replication exists; which flags sync is a design rule (§8.2) |
| Save safety | ❌ | Separate co-op save copy; never write remote progress into a normal save without backup |
| Session UI | ❌ | Config file + hotkeys first; an overlay later |

## 8. Development design

### 8.1 Repo, license, stack

- **Location:** `C:\Users\<user>\Documents\Default Project\projects\sekiro-seamless\` as a git repo (private GitHub repo optional). This file stays at the root.
- **License decision (important):** fromsoftware-rs carries the NON-AI clauses, and AGPL-3.0 forbids adding further restrictions on a combined work. **Mixing sekiro-coop's AGPL code with fromsoftware-rs in one distributed DLL is therefore possibly license-incompatible** (my reading, not legal advice). Decision: **use fromsoftware-rs and write our own code** under MIT OR Apache-2.0. We take *ideas* from sekiro-coop (protocol design, the hook-fed entity registry, HP chain offsets as facts) but copy no code. Reverse-engineered Sekiro structures can be offered upstream to fromsoftware-rs.
- **Toolchain:** Rust stable, `x86_64-pc-windows-msvc`, VS Build Tools (C++). me3 for loading. x64dbg + Cheat Engine **for reading only** (never attached together with our hooks on the same function). Ghidra or IDA Free for static analysis of the user's own `sekiro.exe`.
- **Workspace:**

```
sekiro-seamless/
├── crates/
│   ├── coop-core/      # pure Rust, host-tested: protocol, session FSM, rules (who owns what), save-sync policy
│   ├── coop-net/       # UDP transport, handshake (shared password + key), reliability, heartbeats, reconnect
│   ├── sdt-ext/        # our Sekiro RE: WorldChrMan, ChrIns, puppet API (spawn/move/animate/despawn), AOBs
│   └── coop-dll/       # cdylib: DllMain, fromsoftware-rs tasks, glue core <-> game
├── tools/
│   ├── peer-sim/       # fake second player: replays recorded snapshots over UDP
│   └── recorder/       # in-DLL feature: record own pos/anim to a file for replay tests
├── profiles/sekiro-coop.me3
├── docs/ (RE-NOTES.md with every offset + how it was verified, HANDOFF.md)
└── FEASIBILITY.md
```

### 8.2 Seamless rules (the behaviour the user asked for)

The core idea: **the session belongs to our mod, not to the game.** It is a UDP link plus heartbeat that lives for as long as both games are running. Game events never close it; they only change the state of the puppet on the other side.

| Event | Rule (default; adjustable later) |
|---|---|
| Either player dies | Session stays. Sekiro's own Resurrection works as normal. On a real death the puppet plays its death animation and is hidden on the other screen until that player respawns, then reappears at their idol. **No re-invite.** |
| Host dies (no resurrection) | Host world resets enemies as in vanilla (host is world authority). Guest stays connected and sees the reset. |
| Guest dies | Only the guest respawns; enemies stay host-driven. |
| Idol rest/warp, area load | Puppet despawns on load start and re-spawns when that player is in-world again. If players are in different maps, both just see "partner in <area>" until they meet. A later feature: "warp to partner's idol". |
| Boss fight | Host owns the boss. Fog gate entry syncs via flags; both may be inside. Boss HP, posture and phase come from the host; the guest's hits are forwarded. Kill flags replicate to the guest only if the guest was present (ERSC-style), so nobody's save gets progress they didn't earn unless they opt in. |
| Disconnect | Auto-reconnect for 60 s; then the puppet disappears and the game keeps running solo. Rejoin is just "start the mod again" (same password). |
| Saves | At session start the mod copies the save to a dated backup. The host's save is authoritative for world state; the guest's save only receives agreed flags (boss kills witnessed, items picked up by themselves). |

### 8.3 Phases with acceptance criteria

Each phase ends with a test the principal can run and judge by eye ("yes/no"), plus a log line. Nothing moves to the next phase on "should work".

**Phase 0: toolchain + clean install (1 evening).**
Done when: §6 setup is complete; a "hello" DLL built from our repo loads via me3 on the original v1.06 exe and writes `coop: in-world tick N` to `%LOCALAPPDATA%\sekiro-coop\log.txt` once per second while playing. The game runs 10 min without crashing.

**Phase 1: puppet spike = go/no-go (time-box: 2–4 weekends).**
1. Port DS3 `WorldChrMan`/`ChrIns` to `sdt-ext`; read our own player position. Pass: the logged XYZ matches SekiroTool/CE.
2. Find Sekiro's debug character creator (`CSDebugChrCreator` analog) and spawn a character at player + 2 m on hotkey F9. Fallback: take over an existing NPC (AI off via SekiroTool's method).
3. Move it visibly: find the position write that rendering honours (physics/Havok proxy, or SekiroTool's teleport path). Pass: F10 moves the puppet 5 m forward on screen.
**Go criteria:** a second character visible, AI off, movable on command, 10 min without a crash. **No-go:** not reached within the time-box → stop the project, document findings, wait for the official mod.

**Phase 2: puppet mirrors a player, offline (2–6 weeks).**
`recorder` saves 60 s of our own position, rotation and animation ID; a second run replays it on the puppet. Pass: the puppet walks, runs, jumps and swings the sword in sync with the recording; it is recognisably "Wolf doing what I did". This proves animation playback with no networking risk.

**Phase 3: two real games, free roam (1–2 months).**
`coop-net` plus `peer-sim` first, then the friend's PC over Tailscale. Pass: both see each other moving and fighting in the same area at under 150 ms delay; the session survives one player dying, resting at an idol and warping, **with no reconnect needed** (the seamless core).

**Phase 4: shared enemies (2–4 months).**
Host-authoritative enemy state (HP, posture, position, death); guest hits forwarded; enemies can target the puppet. Pass: both fight the same Ashina soldier, the posture bar matches on both screens, and the deathblow by either player kills it on both.

**Phase 5: bosses, progress, saves (3–6+ months).**
Pass: Gyoubu Oniwa fought together from fog gate to kill; phases match; the kill is recorded per §8.2; saves are backed up and loadable solo afterwards.

### 8.4 Development loop and tooling

- **Who does what:** Claude writes all code, RE notes and test scripts; the principal builds (one command), launches, plays to a checkpoint and reports "yes/no + log". Batch changes per restart, since each restart costs about a minute plus menu navigation.
- **Test rig:** `peer-sim` replays recorded snapshots so one PC is enough until Phase 3. The friend's PC is the real second instance from Phase 3 on.
- **Every offset goes into `docs/RE-NOTES.md`** with an AOB, how it was found, and how it was verified. AOB-based resolution only, with no hardcoded addresses outside the scanned table. All memory walks are gated on "in-world for N frames" (sekiro-coop's crash lesson).
- **Never** run Cheat Engine hooks or Sekiro Online in the same process as our DLL.

## 9. Hardest unknowns (ranked)

1. **Making a second character appear and move smoothly.** Every open project is stuck here. Phase 1 exists to answer it cheaply.
2. **Can a puppet play player animations** (combos, deflects, prosthetics, grapple)? The player uses a player-only behaviour graph; NPC puppets may lack some animations. A possible fallback is a player-model NPC (`c0000`) with the player TAE.
3. **Enemies fighting two targets.** Sekiro's AI is built around one target; aggro on the puppet may need deep AI hooks.
4. **Boss and death flows**: resurrection, deathblows and multi-phase scripted bosses.
5. **Dormant DS3 phantom code** could make (1)–(3) much easier if revived, but it's undocumented. It must be clean-room, from the game binary only, never from LukeYui's mod.

## 10. Honest effort and risk estimate

- **Phase 1** is the real feasibility test. For an expert FromSoft reverser it takes days to a couple of weeks. For us (AI writes, principal tests, no interactive debugger session for the AI) it takes **weeks**, with real risk of failure.
- **Phases 2–5:** realistically **many hundreds to thousands of hours**. Calibration: LukeYui, who already shipped Sekiro Online, has worked on the Sekiro Seamless rewrite for 1.5+ years without a release; both open attempts are stuck at the puppet.
- **Machine and save risk:** low with me3 (no game files replaced), a backup, save backups and offline play. No ban risk, since Sekiro has no official online play.
- **Legal/ethical:** fine when clean-room, with no crack and no App ID spoofing, and respecting the licenses (§8.1).
- **Probabilities (my judgment, not measured):** Phase 1 succeeds about 50–60%; playable free-roam co-op with seamless deaths and warps (Phase 3) about 25–30%; Elden-Ring-Seamless-level boss and enemy co-op (Phase 5) **about 10%**.

## 11. Recommendation: **GO, gated.** Start development, but Phase 1 is a hard go/no-go

The user wants true seamless co-op, and the one part of "seamless" we fully control (a session that never ends on death, warp or boss) is the *easy* part with our own network layer. The risk sits entirely in the puppet. Phase 1 tests that for 2–4 weekends at near-zero cost and no risk to the install. If the go criteria are not met, stop and wait for LukeYui's official Sekiro Seamless Co-op (Patreon / Nexus `yuiamoroll`).

### Exact first steps

1. **User:** do §6 steps 1–7 (backup, remove extra files, Verify integrity, check 1.6.0.0, me3, Tailscale).
2. **User (optional, 5 min):** open an issue on https://github.com/mstampfli/sekiro-coop asking the author to add the AGPL LICENSE file and whether the project is alive. Ask the fromsoftware-rs maintainer to confirm the NON-AI clause permits AI-assisted development.
3. **Claude:** create the repo skeleton from §8.1 (`git init`, workspace, `coop-dll` depending on `sekiro = "0.14"` from crates.io, `profiles/sekiro-coop.me3` with `[[supports]] game = "sekiro"` and one `[[natives]]`).
4. **Claude + user:** Phase 0 checkpoint: build, launch via me3, confirm the `in-world tick` log line.
5. **Claude:** read SekiroTool's source (MIT, read only) for player position, teleport and AI-disable offsets; port fromsoftware-rs DS3 `world_chr_man.rs`/`chr_ins.rs` and ER `CSDebugChrCreator` to `sdt-ext`.
6. Phase 1 steps 1–3, stop at the go/no-go criteria, and report.

## Sources

- fromsoftware-rs: https://github.com/vswarte/fromsoftware-rs (`crates/sekiro/src/rva.rs`, `rva/rva_data.rs`, `sprj/*.rs`, `crates/eldenring/src/cs/world_chr_man.rs`, `LICENSE-*-NON-AI`, `examples/debug-line`)
- sekiro-coop: https://github.com/mstampfli/sekiro-coop (`README.md`, `docs/HANDOFF.md`, `docs/GAPS.md`, `Cargo.toml`)
- unseamless-coop: https://github.com/micthiesen/unseamless-coop (`README.md`, `docs/ARCHITECTURE.md`, `docs/STATE.md`, `docs/LOCAL-DUO.md`, `LICENSE`)
- me3: https://github.com/garyttierney/me3 (`README.md`, `CHANGELOG.md`, `docs/blog/posts/arxan-reversing-1.md`, `docs/user-guide/troubleshooting.md`, `crates/launcher/src/game.rs`, `crates/cli/src/commands/launch.rs`, `crates/mod-host/src/alloc_hooks/mimalloc.rs`, `crates/mod-host/src/savefile.rs`, `schemas/mod-profile.md`)
- dearxan: https://github.com/tremwil/dearxan
- DS3 network code present in Sekiro: https://github.com/tremwil/ds3-nrssr-rce
- HKMP: https://github.com/Extremelyd1/HKMP
- Sekiro modding wiki: https://github.com/SekiroResurrection/modding-wiki
- SekiroTool: https://github.com/borgCode/SekiroTool
- Sekiro Mod Engine: https://www.nexusmods.com/sekiro/mods/6 ; architecture summary https://deepwiki.com/katalash/ModEngine
- LukeYui status: https://www.patreon.com/LukeYui/posts/update-current-129620429
- Sekiro Online press: https://www.pcgamer.com/sekiro-multiplayer-mod-adds-souls-style-co-op-and-pvp-invasions-included/ , https://kotaku.com/sekiro-multiplayer-mod-lets-friends-die-horribly-toget-1843265922
- Offline enemy placement: https://www.nexusmods.com/sekiro/articles/279
- Local evidence (read only): `C:\Users\<user>\Downloads\SSDT_Fix_Repair_Steam\` (`sekiro.exe` PE header, `cream_api.ini`), `C:\Users\<user>\Downloads\Sekiro-Online-v2.05-577-2-05-1623599819\readme.txt`
