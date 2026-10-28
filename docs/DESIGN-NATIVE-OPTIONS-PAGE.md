# Native "Co-op Settings" page on the game's Options list (Opus design report, 2026-10-26)

Source: new headless IDA runs, outputs in `F:\Temp\ida_opt\o1..o12` (`summary.txt` + `f_<ea>.c` each), DB copy `F:\Temp\ida_opt\db.i64`,
runner `F:\Temp\ida_opt\j3.py`, job file `F:\Temp\ida_opt\job.txt`. Command:
`"C:\Program Files\IDA Professional 9.3\idat.exe" -A -S"F:\Temp\ida_opt\j3.py" -L"F:\Temp\ida_opt\lN.log" "F:\Temp\ida_opt\db.i64"` (about 3 min per job).
Tags: [VAR] = read in the decompile, [ARV] = inferred. Owner request: a native-looking menu page on the game's Options list (NOT ImGui), with all
mod settings that make sense. RVA = address - 0x140000000.

## (a) Verdict
Doable with **hijack + own rows**: keep the native "Network Settings" OptionTop entry (kind 9 [ARV]), relabel it with the existing `0x1410AA310`
text detour, replace only its **page builder** with ours. Ours builds the page with the game's generic page object and combo-row helper, so look,
D-pad/A/B and mouse are native. Each row's getter/setter/value-list are MSVC `std::function`s with vtables implemented in Rust.
Rejected: re-binding the 5 native rows only (3 usable combo rows, too few); a new OptionTop kind (needs a new case in a 50 KB ctor switch + page factory).
Risk medium; start with a log-only probe; everything behind a kill switch.

## (b) Hook points and structures
**OptionTop list (`0x140CB5E30`, CSCommonOptionUnit ctor).** Visible order = u32 array `*(unit+2744)`, entries at +8, count at +56, read via `0x140E1F120` [VAR].
Kind -> label / help text ids (FDP_MenuText / FDP_LineHelp): 0: 110000/110600; 1: 110001/110601; 2: 110700/110605; 3: 110100/110602;
4: 110400/110604 (language dialog); 5: 110500 (Quit); 6: 111030 (own dialog `0x140CBF110`); 7: 111024 (own dialog `0x140CC30F0`); 8: 111000; 9: 111100.
Kinds 0-4, 8, 9 share a job lambda (vtable `0x142AC74E8`, `_Do_call` `0x140CD2740`): plays a sound, opens the page via `0x140CC4B50`, which calls the
**page factory `0x140CC4FE0(kind, a2, a3, std::function* a4)`**. Factory cases: 0 `0x140CCD180`, 1 `0x140CCB0F0`, 2 `0x140CCDED0`, 3 `0x140CCB850`
(looks like Graphics), 4 `0x140CCD3E0`, 8 `0x140CCEDA0`, **9 `0x140CCE010`**.
**Network page = kind 9 [ARV; the probe must confirm].** Title 111100, five rows labels 111110-111114 (`0x140CC68C0`, `...6860`, `...6800`, `...67A0`, `...6740`).
Setters call a net/online singleton at `*0x143EA0BF8` (`0x140D5B110`/`0x140D5B160`, then `0x1411B4C40`); getters read page fields +4694/+4695.
**Builder `0x140CCE010(page_parent a1, a2, std::function* a3) -> page*`** [VAR]. Prologue 12 bytes `40 55 56 57 41 54 41 55 41 56 41 57` (then `lea rbp,[rsp-500h]`),
no RIP-relative code (same shape as `QUIT_MENU_PROLOGUE`). Steps: (1) `page = 0x1419DE730(4696, 8, *0x143D9CE28)` (alloc); (2) `0x140CB8980(page, a1, a2)`
(generic CSOptionPageComponent ctor, vtable `0x142AC5C80`); (3) `page[4693] = 1`; (4) add rows; (5) `0x140CC8410(page)` ("default" button row, label 40330 help 40500);
(6) `0x140CA0AB0(page, 0)` finalise; (7) **destroy a3**: `0x140CBD4E0(a3)`; if `*(a3+56)` non-null call that impl's vtable slot 4 (+32) with `(impl, impl != a3)`.
Our replacement must do the same 7 steps with our rows.
**Combo row helper `0x140CADBA0`** [VAR]: `(page, label176*, int* type /*=11*/, LayoutName* /*{u32 7, u8 0, wchar* L"02_201_CommandList"}*/, fn<u8()>* getter,
fn<void(u8)>* setter, fn<ComboItemList_uint8()>* list, fn* extra /*empty*/)`. Copies the functions (`0x140CB39A0`, `0x140CB3F10`), builds view+controller
(`0x140CB10D0`, `0x140CB1610`), adds the row with `0x140C99370`.
**Labels.** A label is a 176-byte struct filled by a label getter such as `0x140CC68C0(buf)`: label via `0x140C7B910(buf+64, id)`, help via `0x140C7B110(buf+120, id)`
(banks 200/232/272 and 201/233/273); destroyed with `0x140C609A0(buf)`. Text is resolved through `0x1410AA310` (already detoured in quitblock.rs).
**std::function layout (MSVC)**: 64 bytes, inline storage 56 bytes, impl pointer at +56. Impl vtable: 0 `_Copy(this, where) -> impl`, 1 `_Move`, 2 `_Do_call(this, args...)`
(a returned class goes through a hidden ret pointer: `(this, ret*) -> ret*`), 3 `_Target_type`, 4 `_Delete_this(this, bool)`, 5 `_Get`. Our impl = `{vptr, row_id:u64}`
stored inline; `_Copy`/`_Move` write those 16 bytes to `where`; `_Delete_this` does nothing.
**Combo item list (`ComboItemList_uint8`)**: example builder `0x140CFDA60` (On = 40400, Off = 40401). Count at `list+3848`, capacity **32 items** (`0x140CFF3C0` refuses more),
each item 120 bytes: `{u8 value; +8 = *lbl; +16 = wstring copy of lbl+8 via 0x1401C7130; +64 = 0; +72 = wstring(&word_143239970, alloc 0x140C7CD60()) via 0x1401C8720}`; append with
`0x140CFF3C0(list, &item)`, destroy the item with `0x140CBBC30`, the label with `0x14066C870` plus the inline wstring free. For On/Off rows our list `_Do_call` can call the
native `0x140CB0CC0(ret, ...)` path (row 1's list) and get the game's own On/Off strings.
**Text injection.** Extend `quitblock::msg_detour` with a reserved ID range (e.g. `0x3B9A_0000 + n`) returning our cached UTF-16 even when the original returns 0, plus a one-shot
"override" slot so that calling a native label getter (`0x140CC68C0`) while armed yields our row label+help. Relabel 111100 -> "Co-op Settings" in the menu bank and the
help bank (help: "Co-op mod settings. The host sets the gameplay rules.").

## (c) Rows (all type 11 combos, uint8 index; pct = 0..300 step 10 (31 items) + 1 slot for an off-grid ini value (32 max))
Authority: **H** = host edits (client sees the host's value as a 1-item list; scaling and death_debuffs travel in ScaleCfg); **H\*** = gameplay rule read per machine today:
editable only when not connected (Solo), greyed as a 1-item list in a session, applies next session; **L** = local, live. Setter = same edit as the ImGui panel (`menu` EDIT slot ->
puppet.rs applies -> `settings::write_keys` -> ScaleCfg resend); local keys go to `write_keys` + the tuning atomics. Getter reads the in-memory settings snapshot.

| ini key | Label | Values | Authority |
|---|---|---|---|
| enemy_health_scaling / enemy_posture_scaling | Enemy HP / Enemy posture +% | pct | H |
| boss_health_scaling / boss_posture_scaling | Boss HP / Boss posture +% | pct | H |
| death_debuffs | Death debuff | On/Off | H |
| share_loot, share_exp, share_key_items, share_resurrect_unlock | Share loot / Sen+XP / key items / Resurrection | On/Off | H* |
| boss_spectate, boss_reward, death_refill, join_rest, boss_start_gate, boss_persist_on_death | (same names, plain English) | On/Off | H* |
| merge_quests | Quest progress | Off / Merge / Host (0-2) | H* |
| quest_choices_host_only, shared_npc_intros, sync_posture | ... | On/Off | H* |
| block_quit_in_coop, block_ng_in_coop, no_pause_in_coop, allow_cross_cycle | ... | On/Off | H* |
| party_hp_bars, show_status_overlay | Party HP bars / Status line | On/Off | L |
| party_hp_position | HP bar position | Head / Left / Top | L |
| party_hp_content | HP bar shows | Name+HP / HP / Numbers | L |
| party_hp_scale | HP bar size | 50..200 step 10 | L |
| overhead_player_display | Name above player | 0-2 | L |
| spectate_camera, spectate_orbit | Spectate camera / orbit | 0-2 | L |
| spectate_lock, auto_follow | ... | On/Off | L |
| f11_pad_button | Pad menu button | Back / Start / L3 / R3 / None | L |
| dialog_default_yes, confirm_items, custom_icons, native_notices | ... | On/Off | L |
| fps_unlock, vsync | ... | On/Off | L (may be start-up only: check) |

Left out on purpose: network text (password, name, address, port: no text entry in a combo row), raw key codes (`*_key`), [DEBUG]/[ADVANCED] (enemy_mode, ghost_*, hz values, n0_clones...),
start-up-only keys (observe, gpu_guard, overlay_hook). About 40 rows. **Scrolling capacity of the "02_201_CommandList" page is unknown** (Graphics has many rows, so it probably scrolls [ARV]);
if it does not scroll, cut to H + L.

## (d) Plan
**v8.34-probe (log-only; the owner runs it once, ~2 minutes in the menu):** detour `0x140CB5E30` (same 12-byte prologue, post-call) and log `optprobe: OptionTop kinds = [...]`; detour factory
`0x140CC4FE0` (prologue `48 8B C4 4C 89 48 20 57 48 83 EC 70`, 12 bytes, no RIP code) and log `optprobe: page kind K opened`; in `msg_detour` log each watched ID once with its text:
`optprobe: text bank B id N = "..."` for 110000..111114, 40400/40401, 40330. Live acceptance: kind 9 = "Network Settings", row labels and list order. Run at the title screen and in the pause menu.
Also (for the Remnant-dialog side question): log every `0x1410AA310` ID requested within 2 s after `input: gamepad button triggered F11`.
**T1 (pure model, new `optmenu_model.rs`, unit tests only):** row table, value<->index mapping, off-grid slot, <=32 cap, role logic (client/connected -> 1-item lists for H/H*), ini update builder (compare with `settings::rewrite_ini` in tests).
**T2 (FFI primitives, new `msfunc.rs` + `nativetext.rs`; also owns the `quitblock.rs` msg_detour edit):** Rust `_Func_impl` vtables for `u8()`, `void(u8)`, `ComboItemList_uint8()`, all in `catch_unwind`; text ID range, override slot, string cache; unit tests drive copy/move/do_call through raw pointers.
**T3 (game glue, new `optmenu.rs`; owns `lib.rs`, `settings.rs`, `tuning.rs`):** install with prologue byte checks on `0x140CCE010`, `0x140CADBA0`, `0x140CB8980`, `0x140CA0AB0`, `0x140C7B910`, `0x140CFF3C0`; replacement builder = the 7 steps with each row through `0x140CADBA0`; item builder + native On/Off list path; ini key `native_mod_menu`.
**Order:** probe -> T1 || T2 -> T3 -> v8.35 live -> v8.36 retire ImGui. Live checks for v8.35: entry reads "Co-op Settings"; page opens/closes, B returns; pad and mouse change values; ini changes and the friend's scaling follows; client cannot change H rows; reopening shows new values; getter refresh rate unknown.
Log lines: `optmenu: 6/6 prologues ok, page builder detoured`; `optmenu: built page with N rows`; `optmenu: set <key> = <v> (role R)`; `optmenu: client tried host row <key>: refused`; on failure `optmenu: OFF (<reason>), native Network page kept`.

## (e) Retiring the ImGui panel
v8.35: `ingame_menu` default -> 0 (settings.rs). Keep ui.rs/menu.rs code; `pred_detour`'s SYSTEM_LIST stamp stays gated by it. Keep `menu::Values`/EDIT/`ini_updates` (the native page reuses them through puppet.rs).
After two good live sessions delete the panel UI from ui.rs and the pad-navigation parts of menu.rs (`apply_nav` and tests).

## (f) Crash safety
`[GAMEPLAY] native_mod_menu` defaults to 0 until v8.35 is verified; when 0, or the mod is not alive, the detour calls the original builder. Every native address is prologue-checked before install; on any mismatch nothing is installed.
Callbacks run on the game thread inside the menu and only touch Rust state (mutex/atomics), never game memory; edits apply in the existing game task. A panic in a callback returns the current/default index and latches OFF for later builds.
If the row helper is not called (prologue or validation fail) fall back to the original `0x140CCE010` via trampoline; a3 must be destroyed exactly once on every path. Unknowns only live play can answer: list scrolling, getter refresh, whether 4693 is needed, whether 111100 is shown elsewhere (title screen options).

## Remnant "not available" dialog (side question)
Not found cheaply. The remnant UI is `AppMenu::BloodMessageTopDialog` (vtable `0x142AB9578`, built in `0x140CA24D0`/`0x140CA29F0`). Next step: probe logs the text IDs requested within 2 s after the pad-F11 line, then an `imm <id>` search finds the function that raises the dialog. Simplest alternative without RE: change the `f11_pad_button` default from Back to `none` or `r3` (our XInput read is passive; it cannot swallow the press).

---
# ADDENDUM (2026-10-26, after the v8.34 probe): CORRECTED FACTS (supersedes the kind-9 mapping above)
IDA outputs: `F:\Temp\ida_opt\o13`, `o14`. **Network Settings = kind 2, NOT kind 9.** Kind 9 = Graphics Options (builder `0x140CCE010`, rows 111110-111114): DO NOT TOUCH.
OptionTop kinds in list order `[0,1,3,6,7,8,9,2,5]` = Game, Camera, Sound&Display, Controller Config (own dialog), Key Config (own dialog), Input Device, Graphics, **Network (kind 2, label 110700 bank 200, help 110605 bank 201)**, Quit.
The `*0x143EA0BF8` online-singleton setters belong to Graphics, not Network.
**Kind 2 builder `0x140CCDED0(a1, a2) -> page*`** (2 args, no a3; factory `0x140CC4FE0` case 2 calls `ccded0(a2, a3)` and destroys its own a4, so OUR builder destroys nothing).
Body: (1) `page = 0x1419DE730(4696, 8, *0x143D9CE28)`, then `0x1419DE750(page, 8, alloc)`; (2) `0x140CB8980(page, a1, a2)` (generic CSOptionPageComponent ctor);
(3) one native row `0x140CAD7C0(page, label, a2+109, list, a2+229, 1)`: label `0x140CC5D10` (110710 / help 110701), list `0x140CFDCF0` (2 items: 0 = text 40320, 1 = text 40321),
bound directly to option-data byte `a2+109` (no getter/setter lambdas, nothing calls the online singleton; probably the launch online/offline setting [ARV]); (4) destroy list items `0x140CBBC30` per 120-byte item;
(5) `0x140CC7FC0(page)` = a "Defaults" button row (`0x140C98F80`, label 40330 / help 40500 via `0x140CC6380`); (6) `0x140CA0AB0(page, 0)` finalise; return page.
Nothing else reads the page state; `page[4693]` is NOT set for kind 2 (kind 9 only); only the factory calls `ccded0`.
**Decision:** drop the native row (`a2+109` untouched: the saved launch setting keeps its value) and drop the Defaults row `0x140CC7FC0` (may reset bound fields [ARV]); our own reset can come later as a combo row.
**Prologue of `0x140CCDED0`:** `40 57 B8 20 10 00 00 | E8 34 C0 68 01 ...` (`push rdi; mov eax,1020h; call _alloca_probe`): the 5th instruction onward is a rel32 call, so detour **7 bytes only** (`install_detour` needs 5-32). `0x140CADBA0` prologue = `48 8B C4 55 41 54 41 55 41 56 41 57` (12 bytes).
## T2 (FFI primitives: `msfunc.rs`, `nativetext.rs`; also owns the `quitblock.rs` msg_detour edit)
- `msfunc.rs`: MSVC std::function = 64 bytes, inline storage + impl pointer at +56. Our impl `{vptr, row_id: u64}`, vtable of 6 `extern "C"` slots: `_Copy(this, where)` and `_Move(this, where)` write the 16 bytes and return `where`;
  `_Do_call` in 3 flavours: `u8 get(this)`, `void set(this, *const u8)`, `*mut List list(this, ret)` (returns `ret`); `_Target_type` -> null; `_Delete_this` no-op; `_Get` -> `this+8`. Every body in `catch_unwind`; on panic return the current index (or 0) and latch `optmenu::FAILED`. Unit tests drive copy/move/do_call through raw pointers.
- `nativetext.rs`: our id range `0x3B9A_0000..+0x1_0000` -> cached never-freed UTF-16 strings. Relabel 110700 (banks 200/232/272) to "Co-op Settings" and 110605 (banks 201/233/273) to "Co-op mod settings. The host sets the gameplay rules."
  Page title: the page shows its own label 110710 or 110700 [ARV]; override 110710 to "Co-op Settings" only while our builder runs (the probe log shows which id the title uses). A one-shot override slot `(label, help)` answers the next lookup of 110710/110701 (used while calling the native label getter `0x140CC5D10(buf176)`, then `0x140C609A0(buf)`).
- `quitblock.rs` msg_detour: call `nativetext::lookup(bank, id, orig)` first, return when it gives `Some`, only while `native_mod_menu` is on.
## T3 (page builder replacement: `optmenu.rs`; owns `lib.rs`, `settings.rs`, `tuning.rs`)
- Prologue checks before install: `0x140CCDED0` = `40 57 B8 20 10 00 00`; `0x140CADBA0` = `48 8B C4 55 41 54 41 55 41 56 41 57`; read and check the first bytes of `0x140CB8980`, `0x140CA0AB0`, `0x140CC5D10`, `0x140CFF3C0`, `0x140C609A0` (j3.py `b <ea> 16`). Any mismatch: log `optmenu: OFF (<fn> prologue)` and install nothing.
- Detour `0x140CCDED0(a1, a2)`: if the menu is off/failed/mod not alive: call the trampoline. Otherwise: (1) alloc 4696 + `0x1419DE750` + ctor `0x140CB8980(page, a1, a2)`; (2) for each row in `optmenu_model`: label buffer via `0x140CC5D10` with the override armed, then `0x140CADBA0(page, buf, &11i32, &Layout{7, 0, L"02_201_CommandList"}, &get, &set, &list, &empty64)`, free the buffer with `0x140C609A0`; (3) `0x140CA0AB0(page, 0)`, return page.
- List builder: for On/Off call native `0x140CFDCF0(ret)` (0 = 40320, 1 = 40321) or `0x140CFDA60` (40400/40401); other lists: build items with our text ids following `0x140CFDA60`'s per-item sequence; cap 32 items.
- Logs: `optmenu: Network Settings page replaced: N rows`, `optmenu: set <key>=<v> (role R)`. Live checks: page opens, scrolls and goes back with B (pad and mouse); the `a2+109` launch setting is unchanged after leaving the game; the title text is correct.
## Remnant "Remnant Menu not available" (bank 204 id 321011): NOT solved
`imm 0x4E8B3` finds nothing (id taken from a table or computed). `Open` (`0x1408C4B40(u32 msg, i32 buttons, u32 type)`) callers: `0x14068C270`, `0x14068DF90`, `0x14075B590`, `0x140909870` (x2), `0x140E0D770`, `0x140E18550` (the last two menu-side candidates [ARV]). Open prologue: `48 89 5C 24 08 48 89 6C 24 10 48 89 74 24 18 57` (detour 15 bytes). Skipping Open may hang a caller that waits for an answer: next step = a log-only Open detour logging msg/buttons/type/caller RVA; if 321011 comes from a fire-and-forget menu caller, a flag-gated return for msg == 321011 is the fix. Zero-risk fallback: default `f11_pad_button` to `r3`/`none`.
