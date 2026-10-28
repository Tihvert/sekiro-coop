# IDAPython: decompile the debug-create path below sub_140A075D0 to learn which
# init-data fields (creator+0x100 = a2) feed NpcParam / CharaInitParam / think.
import os
import ida_funcs
import ida_hexrays
import ida_pro
import idc

OUT_DIR = r"F:\SekiroRE\out4"
ROOTS = [0x140A05550, 0x1409CB210, 0x1407CEAF0]
TEXT_LO, TEXT_HI = 0x140001000, 0x14291D000
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
summary, done = [], set()


def dec(ea, label, depth):
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
    if depth < 2:
        ea2 = f.start_ea
        while ea2 < f.end_ea:
            if idc.print_insn_mnem(ea2) == "call":
                t = idc.get_operand_value(ea2, 0)
                if TEXT_LO <= t < TEXT_HI:
                    dec(t, f"{label}c", depth + 1)
            ea2 = idc.next_head(ea2, f.end_ea)


for i, r in enumerate(ROOTS):
    dec(r, f"r{i}", 0)
with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
