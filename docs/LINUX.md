# Sekiro Together on Linux / Steam Deck

**Not tested on Linux by me yet.** I only have Windows. The mod loader (me3) officially supports Linux through Proton and the Steam Deck, and nothing in the mod should need Windows, but nobody has tried this combo yet. If you try it, please tell me how it went, good or bad (GitHub issue or Discord, see the end).

You do **not** need the `WINEDLLOVERRIDES="dinput8.dll=n,b" %command%` launch option that Sekiro Online uses. This mod isn't a dinput8.dll. me3 starts the game itself and loads the mod.

## What you need

- Sekiro on **Steam, version 1.06** (the last patch, normal unmodded exe)
- Steam for Linux running (native or Flatpak) with Proton for Sekiro. Whatever Steam already uses is fine.
- [me3](https://github.com/garyttierney/me3) for Linux (0.13.0 or newer is what I looked at)

## Install

1. Start Sekiro once from Steam the normal way and get to the main menu, then quit. This makes the Proton prefix and the save folder.
2. Got Sekiro Online or other online mods? Delete `dinput8.dll`, `steam_api64.org`, `cream_api.ini` and `SekiroOnlineFont.ttf` from the game folder, remove the `WINEDLLOVERRIDES` launch option, then Steam > Sekiro > Properties > Installed Files > Verify integrity.
3. Install me3. Open a terminal (Konsole on the Deck) and run the one-liner from the me3 docs:
   ```bash
   curl --proto '=https' --tlsv1.2 -sSfL https://github.com/garyttierney/me3/releases/latest/download/installer.sh | sh
   ```
   It puts `me3` in `~/.local/bin`. If it says "me3 is not available on PATH", that's not an error, the install worked. `play.sh` finds it there anyway.
4. Unzip the mod somewhere in your home folder, for example `~/Games/sekiro-together`. **Not** the game folder.
5. With Steam running, start it once:
   ```bash
   cd ~/Games/sekiro-together
   sh play.sh
   ```
   The game starts with the mod. Close it. `sekiro_coop_settings.ini` shows up next to `sekiro_coop.dll`.
   Double-clicking `sekiro-coop.me3` in the file manager should work too (me3 registers `.me3` files), but maybe only after you log out and back in once.
6. Put the same `password = ...` in the ini for everyone. Co-op is off while it's empty.
7. Everyone needs the exact same mod version. Windows and Linux players should be able to play together, it's the same mod.

## Playing

Same as on Windows (see HOW-TO-PLAY.txt), just start it with `sh play.sh` every time. Starting Sekiro from the Steam library button starts the game **without** the mod.

## Steam Deck

- Do the install in **Desktop Mode** (Konsole for the commands, Kate or any text editor for the ini).
- Playing in Desktop Mode with `sh play.sh` is the way me3 supports.
- Game Mode is untested. What should work: in Desktop Mode, Steam > Games > Add a Non-Steam Game > Browse, pick `/home/deck/.local/bin/me3` (switch the file type to all files). Then in that shortcut's Properties set the launch options to `launch -p "/home/deck/Games/sekiro-together/sekiro-coop.me3"` and leave Compatibility off (me3 is a Linux program, it starts Proton itself). If you try it, tell me if it works.
- F11 marks a bug in the log. On the Deck use the View button (the one left of the screen, it's Back on an Xbox pad).
- The mod unlocks the game to 120 fps. If the Deck gets hot or the battery drains fast, set `fps_unlock = 0` in the ini for the normal 60.

## Where stuff is on Linux

The ini and `udp_join_code.txt` are next to `sekiro_coop.dll`, in the folder you unzipped. Everything else is inside Sekiro's Proton prefix, which is a fake Windows `C:` drive:

```
~/.local/share/Steam/steamapps/compatdata/814380/pfx/drive_c/users/steamuser/
  AppData/Local/sekiro-coop/                      log.txt, log.old.txt, save-backups/
  AppData/Roaming/Sekiro/<your SteamID64>/        S0000.sl2 (normal save), S0000.coop.sl2 (co-op save)
```

- `814380` is Sekiro's Steam app id.
- `~/.steam/steam` is usually the same folder as `~/.local/share/Steam`.
- Flatpak Steam: `~/.var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/compatdata/814380/...`
- Game on another drive or the Deck's SD card: the prefix can be in that drive's `steamapps/compatdata/814380` or in the main one above.
- Can't find it? This finds the log folder:
  ```bash
  find ~ /run/media -type d -path '*compatdata/814380/pfx/drive_c/users/steamuser/AppData/Local/sekiro-coop' 2>/dev/null
  ```
- The log's first lines say `platform: Wine ...` with the exact Linux path of the log folder.
- me3's own logs: `~/.local/share/me3/logs`.

Back up `AppData/Roaming/Sekiro` before playing, same as on Windows.

## If it doesn't start

- **`me3: command not found`**: use `sh play.sh`, or add `export PATH="$PATH:$HOME/.local/bin"` to the end of `~/.bashrc` (Deck: `~/.bash_profile`) and open a new terminal.
- **"unable to determine Proton runtime" or "unable to find installation of Proton runtime"**: Steam > Sekiro > Properties > Compatibility > tick "Force the use of a specific Steam Play compatibility tool", pick Proton Experimental (or another recent Proton), start the game once from Steam, try again.
- **"Couldn't find a valid steam installation"** or me3 picks the wrong Steam (you have both native and Flatpak Steam): put `steam_dir = "/home/<you>/.local/share/Steam"` in `~/.config/me3/me3.toml`.
- **Steam not running**: start Steam first. me3 needs it.
- **Game starts but no co-op stuff, no ini file**: check `sekiro_coop.dll` and `sekiro-coop.me3` are in the same folder, then send me what the terminal printed.
- **"Wrong Sekiro version" notice**: the mod only works with Steam 1.06, unmodded.
- **Game starts without the mod**: you started it from the Steam library. Use `sh play.sh`.

## Connecting

- Steam (the default) should work the same as on Windows, Linux and Windows players joining each other.
- Direct UDP: the host needs port **28100 UDP** open. Steam Deck has no firewall. On a PC with a firewall:
  - ufw (Ubuntu, Mint): `sudo ufw allow 28100/udp`
  - firewalld (Fedora, Nobara): `sudo firewall-cmd --add-port=28100/udp` (add `--permanent` to keep it)
- Can't open ports? A virtual LAN works on Linux too, like ZeroTier or Tailscale (Radmin VPN is Windows only). Use the host's VPN IP as `udp_host_address`.

## Things I don't know yet

- If the in-game overlay (status line, HP bars, Co-op Settings page) draws right under DXVK. It should, the overlay library supports Wine.
- If the spectate camera's mouse control feels the same. Controller should be fine.
- Performance. The mod freezes the game's threads for a moment when it patches code. If you see stutter, the log has `hook: freeze slow` lines, send those.

## Tell me how it went

Open an issue on [GitHub](https://github.com/Tihvert/sekiro-coop/issues) or message me on Discord (**Tihvert**). Please say:

- distro or Steam Deck, and the Proton version Sekiro uses
- me3 version (`me3 info`)
- did the game start with the mod, did co-op connect, with whom (Linux or Windows)
- `log.txt` from the folder above, and what the terminal printed if it didn't start
