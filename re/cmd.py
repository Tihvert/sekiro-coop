import os, ida_funcs, ida_hexrays, ida_name, ida_pro, idautils, idc
OUT_DIR = r"F:\SekiroRE\out13"
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
            m = idc.print_insn_mnem(ea2)
            if m in ("call", "jmp"):
                t = idc.get_operand_value(ea2, 0)
                if TEXT_LO <= t < TEXT_HI and t != f.start_ea: dec(t, label + "_c", depth + 1)
            ea2 = idc.next_head(ea2, f.end_ea)
for ea, name in idautils.Names():
    dn = ida_name.demangle_name(name, 0) or name
    if "ComCmd_ForcePlayAnimation" in dn and "vftable" in dn:
        summary.append(f"VFTABLE {ea:#x} {dn}")
        for i in range(6):
            v = idc.get_qword(ea + 8*i)
            if not (TEXT_LO <= v < TEXT_HI): break
            summary.append(f"  [{i}] {v:#x}")
            dec(v, f"cmd{i}")
dec(0x140756AE0, "dispatch")
open(os.path.join(OUT_DIR, "summary.txt"), "w").write("\n".join(summary))
ida_pro.qexit(0)
