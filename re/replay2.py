# IDAPython: replay data object internals (488 bytes, heap qword_143D9CE50).
import os
import ida_funcs
import ida_hexrays
import ida_pro
import idc

OUT_DIR = r"F:\SekiroRE\out7"
TEXT_LO, TEXT_HI = 0x140001000, 0x14291D000
TARGETS = [
    (0x1409A7F70, "data_ctor"),
    (0x1409A8200, "data_deserialize"),
    (0x1409A8150, "data_frame"),
    (0x1407F4C90, "manip_apply_frame"),
    (0x1407F5220, "manip_frame_time"),
    (0x1407F51C0, "manip_total_time"),
    (0x1409A8020, "data_dtor"),
    (0x1407AF640, "appear_apply"),
]
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
summary, done = [], set()


def dec(ea, label, depth=0):
    f = ida_funcs.get_func(ea)
    if f is None or f.start_ea in done:
        return
    done.add(f.start_ea)
    try:
        text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e:  # noqa: BLE001
        text = f"// failed: {e}"
    with open(os.path.join(OUT_DIR, f"{label}_{f.start_ea:x}.c"), "w", encoding="utf-8") as fh:
        fh.write(f"// {label} {f.start_ea:#x}\n{text}")
    summary.append(f"{'  ' * depth}{label} {f.start_ea:#x} len {len(text)}")
    if depth < 1:
        ea2 = f.start_ea
        while ea2 < f.end_ea:
            if idc.print_insn_mnem(ea2) == "call":
                t = idc.get_operand_value(ea2, 0)
                if TEXT_LO <= t < TEXT_HI:
                    dec(t, label + "_c", depth + 1)
            ea2 = idc.next_head(ea2, f.end_ea)


for ea, label in TARGETS:
    dec(ea, label)
with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
