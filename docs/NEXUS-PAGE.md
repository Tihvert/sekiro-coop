# Nexus Mods page (copy-paste)

Replace `https://github.com/Tihvert/sekiro-coop` and `<KOFI_LINK>` before posting. Category: Gameplay (or Multiplayer). Upload `sekiro-seamless-coop-v8.62.zip` as the main file, the 4 pics from `media/` as images (the temple one as the main image).

Permissions tab: assets free to use / modify / convert as long as credited, open source (MIT), donation points on if you want.

## Name

Sekiro Seamless Co-op

## Summary (max 350 characters)

Seamless co-op for Sekiro, Elden Ring Seamless Co-op style. 2-6 players in the same world, shared enemies and bosses, the session survives deaths, warps and loading screens. Beta and still buggy. Open source on GitHub so anyone can keep working on it. Needs me3 and Sekiro 1.06.

## Description (BBCode)

```bbcode
[size=6][b]Sekiro Seamless Co-op[/b][/size]

Seamless co-op mod for Sekiro. Kinda like the Elden Ring Seamless Co-op mod but for Sekiro. You and your friends (2-6 players) play in the same world, fight the same enemies and bosses, and the session doesn't die when someone dies, warps or hits a loading screen.

[size=4][b]Heads up, read this first[/b][/size]
[b]The mod is still buggy.[/b] It's a beta. Me and my buddy played a lot with it and most stuff works, but there's plenty of jank left. Back up your saves.

I already beat the game so my motivation to keep building this dropped a lot. I might still come back to it at some point, no promises. That's why the whole thing is open source on GitHub, including all my debugging notes and tools. If you want to keep going with it, fork it, fix stuff, send PRs, make your own version, go for it.

Full honesty: this was made with a LOT of help from AI (mostly Claude). I can read code a bit but I didn't write most of it by hand. My part was the idea, testing it in game over and over, reading logs and telling the AI what's broken.

GitHub (source, full readme, bug reports): [url=https://github.com/Tihvert/sekiro-coop]https://github.com/Tihvert/sekiro-coop[/url]

[size=4][b]What works[/b][/size]
[list]
[*] 2-6 players, one host and everyone else joins. 2 players tested a lot, 3+ never tested live.
[*] Other players show up as real characters with name, ping and gear.
[*] Enemies and bosses synced, plus bosses, idols, shortcuts and NPC quest progress.
[*] Boss rules: die in a boss fight and you wait or spectate until the boss is dead or everyone is.
[*] Resting respawns enemies for everyone.
[*] Shared XP, sen, drops, boss rewards and key items (can be turned off).
[*] Enemy/boss scaling, optional death debuff, deathblow lock, party HP bars.
[*] Own co-op save file, your normal save isn't touched. Auto backups every 10 min.
[*] Co-op Settings page in the game's own Options menu.
[/list]

[size=4][b]What's broken / not done[/b][/size]
[list]
[*] Resurrect sometimes isn't offered when it should be (fix in 8.61, not tested in game).
[*] Spectate sometimes goes black.
[*] Enemies or bosses can desync now and then.
[*] Lag / stutter spikes, mostly on the host.
[*] 3-6 players never tested live.
[*] Some story/NPC flags can get lost.
[*] Levers, lifts and breakables not synced. Story phases, Dragonrot, Bell Demon and cutscenes not handled specially.
[*] The game can crash sometimes.
[*] Everything from 8.53 to 8.62 hasn't been tested in game.
[/list]

[size=4][b]Requirements[/b][/size]
[list]
[*] Sekiro on Steam, version [b]1.06[/b] (normal unmodded exe)
[*] [url=https://github.com/garyttierney/me3/releases]me3 mod loader[/url]
[*] Windows
[*] Everyone on the exact same mod version
[/list]

[size=4][b]How to install[/b][/size]
[list=1]
[*] Install me3.
[*] Got Sekiro Online or other online mods? Delete dinput8.dll, steam_api64.org, cream_api.ini and SekiroOnlineFont.ttf from the game folder, then Verify integrity in Steam.
[*] Unzip the mod anywhere [b]except[/b] the game folder.
[*] Start Steam, run play.cmd once, close the game. sekiro_coop_settings.ini shows up next to the DLL.
[*] Put the same password for everyone in the ini (password = ...). Co-op stays off while it's empty.
[/list]

[size=4][b]How to play[/b][/size]
[list=1]
[*] Start the game with play.cmd and load your save.
[*] After the intro (Genichiro beats you, you wake up at Dilapidated Temple), rest at the temple idol. You get 3 items: [b]Kindred Bell[/b] (host), [b]Kindred Charm[/b] (join the host) and [b]Severed Thread[/b] (leave co-op).
[*] Host uses the Bell, everyone else uses the Charm. Connection status is in the top left corner.
[*] Settings: the ini or Options > Co-op Settings in game. The host's rules apply to everyone.
[/list]

Connection is Steam by default and falls back to direct UDP after 15 s. For UDP the host needs port 28100 UDP open, the address is in udp_join_code.txt.

[size=4][b]When something breaks[/b][/size]
Log is at %LOCALAPPDATA%\sekiro-coop\log.txt. Press F11 (or Back/Share on a pad) right when a bug happens, it marks the log. Bug reports go on GitHub with everyone's logs, that's way easier for me or anyone else to look at than a Nexus comment.

[size=4][b]Credits[/b][/size]
[list]
[*] [b]naamanorsunvitulla[/b] (same name on Discord), my co-op buddy, played through all of it with me, tested every broken build and sent a ton of logs.
[*] vswarte for fromsoftware-rs (the sekiro crate)
[*] Gary Tierney and contributors for me3
[*] veeenu for hudhook
[*] LukeYui's Elden Ring Seamless Co-op and the DS3 seamless mods for the inspiration
[*] Built with a lot of help from Claude (Anthropic)
[/list]

[size=4][b]Support[/b][/size]
If you like it and want to throw a coffee my way: [url=<KOFI_LINK>]Ko-fi[/url]. Totally optional.

Unofficial fan mod, not affiliated with FromSoftware, Activision or Valve. Doesn't touch the official online mode. Use at your own risk.
```
