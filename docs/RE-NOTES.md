# RE notes: Sekiro 1.06 (1.6.0.0), Steam-verified exe, 68,005,144 bytes

Every entry: what, where, source, and how it was verified on this machine.

## Facts verified in-game (2026-10-22)

| What | Value | Source | Verified by |
|---|---|---|---|
| No Arxan | `dearxan::disabler: arxan detected: false` | me3 log | me3 launch |
| `.text` is encrypted on disk | Bytes at file offset for RVA 0x6C1A10 are random | SteamStub `.bind` section | Reading the exe; code is only readable at runtime |
| WorldChrMan global | RVA `0x3D7A1E0` | SekiroTool `Offsets.cs` | Matches cheat-menu log `WorldChrMan[0x143D7A1E0]`; player position read correctly |
| Player ChrIns | `[WorldChrMan] + 0x88` | SekiroTool | Position matches movement |
| ChrIns modules | `ChrIns + 0x1FF8` → table; `+0x18` data, `+0x68` physics | SekiroTool | HP and position read correctly |
| HP | data module `+0x130` | SekiroTool | Plausible values (wolf 28, soldier 233) |
| Position / angle | physics module `+0x80` (xyz, w at `+0x8C`) / `+0x74` | SekiroTool | Correct |
| Second position copy | `ChrIns + 0x1060` | Our F6 scan | Found 2 copies on a live wolf |
| `GetChrInsByEntityId` | RVA `0x6C1A10`, `ChrIns* f(const int* id)` | SekiroTool asm (`movabs rcx, &id`) | Passing the value crashed at `+0x6C1A29`; the pointer form works and finds 89 characters |
| Flag block | `ChrIns + 0x1F40`: byte0 bit5 NoHit, bit6 NoAttack, bit7 NoMove; byte1 bit4 FreezeAi | SekiroTool | FreezeAi stops a wolf visibly |
| **UpdateCoords store** | RVA `0xBC4796`, `movaps [rsi+0x80], xmm6` (`0F 29 B6 80 00 00 00`), `rsi` = physics module | SekiroTool noclip hook | Bytes match at runtime; our hook moves a live puppet visibly |
| **CSDebugChrCreator** | `WorldChrMan + 0x3148` (`.?AVCSDebugChrCreator@NS_SPRJ@@`); `+0x3150` CSDebugChrPerfChecker | Our F11 RTTI dump | RTTI name read live (full dump: `docs/rtti-dump-2026-10-22.log`) |
| Leftover session objects | `WorldChrManDbg + 0x158 -> PlayerSessionHolder -> +0x18 PlayerNullSession` | RTTI dump | Live |
| Entity id range | 7 digits, map-prefixed (e.g. 1100350–1100951 near the first idol) | Scan | 89 hits in 1,000,000–2,600,000 |

## Behaviour findings

- **Writing physics `+0x80` directly does nothing visible.** The game overwrites it later in the frame at UpdateCoords from Havok.
- **The hook alone is enough** to move a live character. An A/B test with and without a direct write made no difference.
- **FreezeAi + hook does not move the character, and costs a lot of FPS** (60 → 15). Keep AI on and use NoAttack + NoHit only.
- **Respawn reads the hooked position**: after the player's death the re-created wolf spawned at the hook target.
- **Cost of following**: about 2–3 FPS (57–58 → 55–58).
- A full position-copy scan (F6, ~68k `VirtualQuery` calls) stalls the game for about 10 s; it is fine for one-off RE only.

## Not yet known / next

- `ForceAnimation` RVA `0x6D5A90`, `f(int entityId, int animId)` (SekiroTool snake loop: `ecx=1100850, edx=20000`). Not yet called by us.
- Making the puppet look like Wolf (`c0000`), via the debug chr creator or a player-model NPC.
- The puppet has no animations while moved (slides).
- Facing: the puppet's angle is not set yet (only xyz goes through the hook).
