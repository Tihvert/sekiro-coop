# IDAPython: how ReplayManipulator dresses a ghost (apply-frame sub_1407F4C90):
#   v21 = chr->vtbl[752/8](chr)          per-chr appearance/equip holder
#   sub_140BF1410 -> v21+8..+36, sub_1409F6780(chr, *(u32*)(v21+8))
#   for slot in unk_1429D5978[4]: sub_1409CCCB0(v21, slot, &id)
import os
import ida_bytes
import ida_funcs
import ida_hexrays
import ida_pro

OUT_DIR = r"F:\SekiroRE\out8"
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
summary = []
slots = [ida_bytes.get_dword(0x1429D5978 + 4 * i) for i in range(4)]
summary.append(f"slots at 0x1429D5978: {slots}")
pv = ida_bytes.get_qword(0x142A2B338 + 752)  # PlayerIns vftable entry 94
gv = ida_bytes.get_qword(0x142A2D190 + 752)  # ReplayGhostIns vftable entry 94
summary.append(f"PlayerIns vft[94] = {pv:#x}, ReplayGhostIns vft[94] = {gv:#x}")
for ea, label in [(0x1409CCCB0, "set_equip"), (0x1409F6780, "set_look"), (0x140BF1410, "look_struct"),
                  (pv, "player_vft94"), (gv, "ghost_vft94"), (0x1408D6250, "frame_equip"), (0x1408D6240, "frame_look")]:
    f = ida_funcs.get_func(ea)
    if f is None:
        summary.append(f"{label}: no func at {ea:#x}")
        continue
    try:
        text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e:  # noqa: BLE001
        text = f"// failed: {e}"
    with open(os.path.join(OUT_DIR, f"{label}_{f.start_ea:x}.c"), "w", encoding="utf-8") as fh:
        fh.write(f"// {label} {f.start_ea:#x}\n{text}")
    summary.append(f"{label} {f.start_ea:#x} len {len(text)}")
with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
