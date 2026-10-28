# IDAPython: decompile the function containing a crash address plus its callers.
import os
import ida_funcs
import ida_hexrays
import ida_pro
import ida_xref
import idc

OUT_DIR = r"F:\SekiroRE\out3"
CRASH = 0x1409CBF09  # read of 0x24 right after a debug-created c0000 (2026-10-22)
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
summary = []


def dec(ea, label):
    f = ida_funcs.get_func(ea)
    if f is None:
        summary.append(f"{label}: no function at {ea:#x}")
        return None
    try:
        text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e:  # noqa: BLE001
        text = f"// failed: {e}"
    with open(os.path.join(OUT_DIR, f"{label}_{f.start_ea:x}.c"), "w", encoding="utf-8") as fh:
        fh.write(f"// {label} {f.start_ea:#x}\n{text}")
    summary.append(f"{label} {f.start_ea:#x} len {len(text)}")
    return f


f = dec(CRASH, "crashfn")
if f:
    # Disassembly around the faulting instruction.
    ea = idc.prev_head(idc.prev_head(idc.prev_head(idc.prev_head(CRASH))))
    lines = []
    for _ in range(10):
        lines.append(f"{ea:#x}: {idc.generate_disasm_line(ea, 0)}")
        ea = idc.next_head(ea)
    summary.append("\n".join(lines))
    n = 0
    ref = ida_xref.get_first_cref_to(f.start_ea)
    while ref != idc.BADADDR and n < 6:
        dec(ref, f"caller{n}")
        n += 1
        ref = ida_xref.get_next_cref_to(f.start_ea, ref)
with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
