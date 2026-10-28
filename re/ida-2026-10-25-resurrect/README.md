# IDA decompilation dump for the Resurrect question (2026-10-25)

Static RE of the Steam exe copy (headless IDA, no game running): out/ (search of every access to
PlayerGameData+0x2128..0x213C and summary.txt), out2/, out3/ (decompiled: 0xA34B70 resurrection
condition, 0xA2CC30 HP-0 handler, 0xA36A60 per-frame window, 0xA2BE40 rest, 0xA36070, 0xA23730,
0x7C51A0 debug string "Revive Recast Point"). rev*.py = the IDA scripts. The .i64 database is not kept
(1 GB). Findings and hypotheses H1 / H2: docs/JATKA-TASTA-MULTIPLAYER.md (v8.15 block). NOT verified live.
Other sources: Ela Sekiro CT https://github.com/ElaDiDu/Sekiro-Practice-CT, Yharnam Together
https://github.com/lKaniel/bloodborne-server (docs/NATIVE_NETCODE.md).
