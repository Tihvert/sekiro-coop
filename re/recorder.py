# IDAPython: ReplayRecorder (records the player for bloodstains/Remnants in
# the same bit-packed frame format ReplayManipulator plays back).
import os, ida_funcs, ida_hexrays, ida_name, ida_pro, ida_xref, idautils, idc
OUT_DIR = r"F:\SekiroRE\out11"
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
TEXT_LO, TEXT_HI = 0x140001000, 0x14291D000
done=set(); summary=[]
def dec(ea, label, depth=0, maxdepth=1):
    f = ida_funcs.get_func(ea)
    if f is None or f.start_ea in done: return f
    done.add(f.start_ea)
    try: text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e: text = f"// failed: {e}"
    open(os.path.join(OUT_DIR, f"{label}_{f.start_ea:x}.c"), "w", encoding="utf-8").write(f"// {label}\n{text}")
    summary.append(f"{'  '*depth}{label} {f.start_ea:#x} len {len(text)}")
    if depth < maxdepth:
        ea2 = f.start_ea
        while ea2 < f.end_ea:
            if idc.print_insn_mnem(ea2) == "call":
                t = idc.get_operand_value(ea2, 0)
                if TEXT_LO <= t < TEXT_HI: dec(t, label + "_c", depth + 1, maxdepth)
            ea2 = idc.next_head(ea2, f.end_ea)
    return f
for ea, name in idautils.Names():
    dn = ida_name.demangle_name(name, 0) or name
    if "ReplayRecorder" in dn and "vftable" in dn:
        summary.append(f"VFTABLE {ea:#x} {dn}")
        for i in range(30):
            v = idc.get_qword(ea + 8*i)
            if not (TEXT_LO <= v < TEXT_HI): break
            dec(v, f"rvft{i:02d}", 0, 1)
        ref = ida_xref.get_first_dref_to(ea)
        while ref != idc.BADADDR:
            f = dec(ref, "rref", 0, 0)
            if f:
                r2 = ida_xref.get_first_cref_to(f.start_ea); n = 0
                while r2 != idc.BADADDR and n < 4:
                    dec(r2, "rrefup", 0, 0); n += 1
                    r2 = ida_xref.get_next_cref_to(f.start_ea, r2)
            ref = ida_xref.get_next_dref_to(ea, ref)
open(os.path.join(OUT_DIR, "summary.txt"), "w").write("\n".join(summary))
ida_pro.qexit(0)
