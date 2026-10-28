import os, ida_funcs, ida_hexrays, ida_pro, idc
OUT_DIR = r"F:\SekiroRE\out9"
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
TEXT_LO, TEXT_HI = 0x140001000, 0x14291D000
done=set(); summary=[]
def dec(ea, label, depth=0):
    f = ida_funcs.get_func(ea)
    if f is None or f.start_ea in done: return
    done.add(f.start_ea)
    try: text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e: text = f"// failed: {e}"
    open(os.path.join(OUT_DIR, f"{label}_{f.start_ea:x}.c"), "w", encoding="utf-8").write(f"// {label}\n{text}")
    summary.append(f"{'  '*depth}{label} {f.start_ea:#x} len {len(text)}")
    if depth < 1:
        ea2 = f.start_ea
        while ea2 < f.end_ea:
            if idc.print_insn_mnem(ea2) == "call":
                t = idc.get_operand_value(ea2, 0)
                if TEXT_LO <= t < TEXT_HI: dec(t, label + "_c", depth + 1)
            ea2 = idc.next_head(ea2, f.end_ea)
dec(0x1409CCBA0, "set_equip_impl")
open(os.path.join(OUT_DIR, "summary.txt"), "w").write("\n".join(summary))
ida_pro.qexit(0)
