# IDAPython: replay-data format for ReplayGhostIns.
#  - callers of sub_140A494A0 (WorldChrMan::CreateReplayGhost): how replay data is built
#  - sub_1407F4840 (manipulator gets replay data), sub_1407C4250 (appearance @ +212)
#  - ReplayManipulator vftable 0x1429D5AE8 entries (per-frame playback)
import os
import ida_bytes
import ida_funcs
import ida_hexrays
import ida_pro
import ida_xref
import idc

OUT_DIR = r"F:\SekiroRE\out6"
TEXT_LO, TEXT_HI = 0x140001000, 0x14291D000
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
summary, done = [], set()


def dec(ea, label):
    f = ida_funcs.get_func(ea)
    if f is None or f.start_ea in done:
        return f
    done.add(f.start_ea)
    try:
        text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e:  # noqa: BLE001
        text = f"// failed: {e}"
    with open(os.path.join(OUT_DIR, f"{label}_{f.start_ea:x}.c"), "w", encoding="utf-8") as fh:
        fh.write(f"// {label} {f.start_ea:#x}\n{text}")
    summary.append(f"{label} {f.start_ea:#x} len {len(text)}")
    return f


def callers(ea, label, depth):
    if depth == 0:
        return
    n = 0
    ref = ida_xref.get_first_cref_to(ea)
    while ref != idc.BADADDR and n < 8:
        f = dec(ref, label)
        if f:
            callers(f.start_ea, label + "u", depth - 1)
        n += 1
        ref = ida_xref.get_next_cref_to(ea, ref)


callers(0x140A494A0, "mk", 3)
for ea, label in [(0x1407F4840, "manip_set"), (0x1407C4250, "appear"), (0x1407C4260, "flag"), (0x1407BFE80, "asmctor")]:
    dec(ea, label)
for i in range(40):
    v = ida_bytes.get_qword(0x1429D5AE8 + 8 * i)
    if not (TEXT_LO <= v < TEXT_HI):
        summary.append(f"manip vft: {i} entries")
        break
    dec(v, f"mvft{i:02d}")
with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
