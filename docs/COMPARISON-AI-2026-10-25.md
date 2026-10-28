# Seamless co-op comparison: four AI answers, checked against our code (2026-10-25)

Source: `C:\Users\<user>\Downloads\vertailu seamless.txt` (Gemini, Grok, GPT, DeepSeek; Tihvert's prompts). Their numbers for other mods are claims, not verified here. GPT and DeepSeek cite sources and mark uncertainty; Gemini states numbers without sources (e.g. "Bloodborne +80-100 % boss HP", "ER posture absorption ~15-20 %"), so treat Gemini's figures as unreliable.

## Where the four agree
- HP and posture need separate curves; posture must grow much slower than HP (GPT 1.40x boss posture at 6 players, Grok 2.30x, DeepSeek "scaling the posture bar punishes the skill"; only Gemini wants posture faster than HP).
- Never add deathblow markers; keep phases and their reset rules; per-boss overrides later.
- Enemy damage to players: 0 to +20 % at most, off by default.
- Presets (Easy / Normal / Hard / Vanilla-feel) and an in-game menu are the most requested settings feature; ini-only is the norm in Yui's mods.
- Item dropping: consumables and upgrade materials only, never key items / prosthetics / skills.
- Invasions / PvP: last, opt-in only (Sekiro Online's mandatory invasions were its most disliked design).

## Their gap list vs. what we actually have
| Claimed gap | Our code | Verdict |
|---|---|---|
| No reconnect / session resume | join.rs `RECONNECT_RESUME` (< 120 s, no warp), leave + rejoin = new join (net.rs `session_gen`) | Partly wrong: short reconnects work; resume after a host crash does not |
| No desync detection | audit.rs (valvoja), doctor.rs settings compare, coop-audit report | Wrong: we have detection, but no automatic recovery |
| No NAT traversal / relay | coop-net `upnp.rs`, Steam transport (`transport = auto`, steam.rs lobby) | Partly: UPnP + Steam exist; no relay / room code |
| No in-game settings menu | ini only (hot-reload for many keys) | Right |
| No boss bar sync | persist.rs boss records, livesync / posture lives | Right in part: values sync, no explicit bar check |
| Scaling curve missing | was linear (6 players: boss HP 8.5x, posture 5x) | Right: fixed in v8.9 (diminishing curve) |
| Posture regeneration scaling | not implemented (no known param offset) | Right: needs RE |
| Player power normalisation | NG+ blocked instead | Right |
| Item drop | missing | Right (TODO) |
| Cutscene sync | pause-only (cutscene.rs) | Right (TODO) |

## Ideas worth keeping (not yet scheduled)
- Co-op Gauntlets of Strength / boss rush (DeepSeek): Sekiro-native content, uses the boss table.
- Session dashboard: role, ping, version, connection diagnostics (GPT P0).
- Boss regression test matrix per boss and player count (GPT).
- Host handover (DeepSeek: nobody has it).
- Localization files; a "can this ban me / save safety" FAQ in the README.

## v8.9 scaling (implemented)
Second player unchanged (tested values: enemy HP +60 %, boss HP +150 %, enemy posture +60 %, boss posture +80 %). Each further player adds a share of the previous step: HP 85 %, posture 60 % (scaling.rs `HP_DECAY`, `POSTURE_DECAY`).

| Players | Enemy HP | Enemy posture | Boss HP | Boss posture |
|---|---|---|---|---|
| 2 | 1.60x | 1.60x | 2.50x | 1.80x |
| 3 | 2.11x | 1.96x | 3.78x | 2.28x |
| 4 | 2.54x | 2.18x | 4.86x | 2.57x |
| 5 | 2.91x | 2.31x | 5.78x | 2.75x |
| 6 | 3.22x | 2.38x | 6.56x | 2.84x |

Still above GPT's and Grok's HP proposals at 4+ players; tune the percentages after a 3-player test. Posture regeneration and presets need RE / UI work first.
