# Nexus Mods page (copy-paste)

Repo and Ko-fi links are already filled in. Category: Gameplay. Upload `sekiro-seamless-coop-v1.0-beta.zip` as the main file (version 1.0-beta), the 4 pics from `media/` as images.

## Name

Sekiro Seamless Co-op

## Summary (max 350 characters)

Seamless co-op for Sekiro, Elden Ring Seamless Co-op style. 2-6 players in the same world, shared enemies and bosses, the session survives deaths, warps and loading screens. Beta and still buggy. Open source on GitHub so anyone can keep working on it. Needs me3 and Sekiro 1.06.

## Description (BBCode)

```bbcode
[size=6][b]Sekiro Seamless Co-op[/b][/size]

[b]Read this before you install.[/b] The mod is a beta and it's still buggy. Me and my buddy played a lot with it and most stuff works, but you're gonna run into jank. Back up your saves. If you're looking for a bug free experience, this isn't it (yet).

I already beat the game so my motivation to keep building this dropped a lot. I might still come back to it at some point, no promises. That's why the whole thing is open source, including all my debugging notes and tools. If you want to keep going with it, fork it, fix stuff, send PRs, make your own version, go for it.

Full honesty: this was made with a LOT of help from AI (mostly Claude). I can read code a bit but I didn't write most of it by hand. My part was the idea, testing it in game over and over, reading logs and telling the AI what's broken.

[b]GitHub (source, full readme, bug reports):[/b] [url=https://github.com/Tihvert/sekiro-coop]github.com/Tihvert/sekiro-coop[/url]
If you're enjoying this and want to support me, you can do it on [url=https://ko-fi.com/tihvert]Ko-fi[/url]. Totally optional.


[size=5][b]About[/b][/size]

[size=4][b]Seamless co-op[/b][/size]
Basically Elden Ring Seamless Co-op but for Sekiro. You and up to 5 friends (2-6 players) play in the same world from the temple to the credits. You see each other as real player characters with name, ping and gear, fight the same enemies and bosses, and the session keeps going through deaths, idol warps and loading screens.

[list]
[*] One host, everyone else joins the host's world.
[*] Enemies and bosses synced, plus bosses, idols, shortcuts and NPC quest progress.
[*] Resting respawns enemies for everyone.
[*] Shared XP, sen, drops, boss rewards and key items (can be turned off).
[*] Co-op uses its own save file, your normal save isn't touched. Auto backups every 10 min.
[/list]

[size=4][b]Dying and bosses[/b][/size]
[list]
[*] If you die in a boss fight you wait or spectate the others until the boss is dead or everyone is. The spectate camera can switch between alive players.
[*] Resurrection works like normal. If everyone dies it's a wipe.
[*] A boss keeps lost health and lives when only one player dies.
[*] Deathblow lock, so two people can't deathblow the same enemy at once.
[/list]

[size=4][b]Balancing[/b][/size]
Enemies and bosses get more HP and posture in co-op, and there's an optional death debuff (each co-op death lowers max HP and posture a bit, resting clears it). All of it is adjustable, and the host's values apply to everyone.

[size=4][b]Quality of life[/b][/size]
[list]
[*] Party HP bars for the other players.
[*] Co-op Settings page inside the game's own Options menu.
[*] Optional 120 fps unlock.
[*] Custom icons for the co-op items.
[/list]

[size=4][b]What's broken / not done[/b][/size]
[list]
[*] Resurrect sometimes isn't offered when it should be ((there's a fix, not tested in game yet)).
[*] Spectate sometimes goes black.
[*] Enemies or bosses can desync now and then.
[*] Lag / stutter spikes, mostly on the host.
[*] 3-6 players never tested live, only 2.
[*] Some story/NPC flags can get lost.
[*] Levers, lifts and breakables not synced. Story phases, Dragonrot, Bell Demon and cutscenes not handled specially.
[*] The game can crash sometimes.
[*] The last batch of fixes before 1.0 beta hasn't been tested in game.
[/list]


[size=5][b]Can this ban me?[/b][/size]
Sekiro doesn't have any online mode or anti-cheat, so there's nothing to get banned from. The mod uses its own connection and its own save file (S0000.coop.sl2), it doesn't touch the official servers.


[size=5][b]Installation[/b][/size]
You need:
[list]
[*] Sekiro on Steam, version [b]1.06[/b] (the last patch, normal unmodded exe)
[*] [url=https://github.com/garyttierney/me3/releases]me3 mod loader[/url] (Windows installer, default options are fine)
[*] Windows
[/list]

[list=1]
[*] Install me3.
[*] Got Sekiro Online or other online mods? Delete dinput8.dll, steam_api64.org, cream_api.ini and SekiroOnlineFont.ttf from the game folder, then Verify integrity of game files in Steam.
[*] Unzip the mod anywhere [b]except[/b] the game folder.
[*] Start Steam, run play.cmd once, close the game. sekiro_coop_settings.ini shows up next to the DLL.
[*] Put the same password for everyone in the ini (password = ...). Co-op stays off while it's empty.
[*] Everyone needs the exact same mod version.
[/list]

Other mods haven't been tested with this. Stuff that doesn't touch multiplayer or the co-op items might work, no guarantees.


[size=5][b]Use[/b][/size]
[list=1]
[*] Start the game with play.cmd and load your save.
[*] After the intro (Genichiro beats you and you wake up at Dilapidated Temple), rest at the temple idol. You get 3 items:
[b]Kindred Bell[/b] - open your world (host)
[b]Kindred Charm[/b] - find the host's world and join it
[b]Severed Thread[/b] - leave co-op
[*] Host uses the Bell, everyone else uses the Charm. Connection status is in the top left corner.
[*] To stop, use the Severed Thread or quit to the title screen.
[/list]

Settings: edit the ini or use Options > Co-op Settings in game.
Connection is Steam by default and falls back to direct UDP after 15 s. For UDP the host needs port 28100 UDP open (UPnP is tried), the address is in udp_join_code.txt.


[size=5][b]Troubleshooting[/b][/size]
[b]Game doesn't start:[/b]
- Is Steam running? play.cmd tells you if not.
- Is me3 installed? play.cmd tells you that too.
- Verify game files in Steam and make sure no other online mods are left in the game folder.

[b]Can't connect:[/b]
- Same password and same mod version on everyone.
- For UDP the host's port 28100 needs to be open.

[b]Something broke in game:[/b]
- The log is at %LOCALAPPDATA%\sekiro-coop\log.txt
- Press F11 (or Back/Share on a pad) right when the bug happens. It marks the log so it's way easier to find later.
- Report it on GitHub with everyone's logs. A Nexus comment with no log is really hard to do anything with.


[size=5][b]FAQ[/b][/size]
[b]Q) Isn't this just easy mode?[/b]
A) Kinda, co-op makes it easier. That's why enemies and bosses get scaled up. The point is playing the whole game with friends, not skipping the challenge.

[b]Q) Can I use my normal save?[/b]
A) Yes. The first time, your normal save gets copied into the co-op save. After that they're separate.

[b]Q) Can I modify or redistribute this?[/b]
A) Yes. It's open source (MIT). Fork it, fix it, upload your own version, just give credit.

[b]Q) Does it work on pirated copies?[/b]
A) No, and I won't support those.

[b]Q) Will you keep updating it?[/b]
A) Maybe. No promises. That's why the source is out there.


[size=5][b]Special thanks[/b][/size]
[list]
[*] [b]naamanorsunvitulla[/b] (same name on Discord), my co-op buddy. Played through all of it with me, tested every broken build and sent a ton of logs. This wouldn't exist without them.
[*] vswarte for fromsoftware-rs (the sekiro crate)
[*] Gary Tierney and contributors for me3
[*] veeenu for hudhook
[*] LukeYui's Elden Ring Seamless Co-op and the DS3 seamless mods for the inspiration
[*] Claude (Anthropic) for writing most of the code with me
[/list]

Unofficial fan mod, not affiliated with FromSoftware, Activision or Valve. Use at your own risk.
```
