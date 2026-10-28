import os, ida_bytes, ida_funcs, ida_hexrays, ida_pro, idc
OUT_DIR = r"F:\SekiroRE\out14"
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
TEXT_LO, TEXT_HI = 0x140001000, 0x14291D000
summary=[]; done=set()
def dec(ea, label, depth=0):
    f = ida_funcs.get_func(ea)
    if f is None or f.start_ea in done: return
    done.add(f.start_ea)
    try: text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e: text = f"// failed: {e}"
    open(os.path.join(OUT_DIR, f"{label}_{f.start_ea:x}.c"), "w", encoding="utf-8").write(f"// {label}\n{text}")
    summary.append(f"{'  '*depth}{label} {f.start_ea:#x} len {len(text)}")
    if depth < 2:
        ea2 = f.start_ea
        while ea2 < f.end_ea:
            if idc.print_insn_mnem(ea2) in ("call", "jmp"):
                t = idc.get_operand_value(ea2, 0)
                if TEXT_LO <= t < TEXT_HI and t != f.start_ea: dec(t, label + "_c", depth + 1)
            ea2 = idc.next_head(ea2, f.end_ea)
for vt, name in [(0x142A2B338, "player"), (0x142A2D190, "ghost")]:
    v = ida_bytes.get_qword(vt + 56)
    summary.append(f"{name} vft[7] = {v:#x}")
    dec(v, f"{name}_vf7")
open(os.path.join(OUT_DIR, "summary.txt"), "w").write("\n".join(summary))
ida_pro.qexit(0)
