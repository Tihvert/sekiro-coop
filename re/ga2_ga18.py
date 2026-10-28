# pass 14 -> F:\SekiroRE\out45 : ReplayManipulator vtable; who consumes +864 (velocity) / writes move input (+16)
import os, ida_funcs, ida_hexrays, idautils, idc, ida_bytes, ida_name, ida_segment
OUT_DIR = r"F:\SekiroRE\out45"; os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
summary, done = [], set()
def is_code(ea):
    s = ida_segment.getseg(ea); return s is not None and (s.perm & ida_segment.SEGPERM_EXEC) != 0
for ea, name in idautils.Names():
    dn = ida_name.demangle_name(name, 0) or name
    if dn in ("const NS_SPRJ::ReplayManipulator::`vftable'", "const NS_SPRJ::ChrManipulator::`vftable'"):
        summary.append(f"VT {dn} {ea:#x}")
        for i in range(60):
            v = ida_bytes.get_qword(ea + 8 * i)
            if not is_code(v): break
            f = ida_funcs.get_func(v)
            if not f: continue
            try: t = str(ida_hexrays.decompile(f.start_ea))
            except Exception as ex: t = f"// failed {ex}"
            tags = [k for k in ("+ 864", "+ 880", "+ 832", "+ 16)", "+ 856", "+ 288") if k in t]
            summary.append(f"  [{i}] {v:#x} len {len(t)} {tags}")
            if f.start_ea not in done:
                done.add(f.start_ea)
                open(os.path.join(OUT_DIR, f"vt{i:02d}_{f.start_ea:x}.c"), "w", encoding="utf-8").write(t)
open(os.path.join(OUT_DIR, "summary.txt"), "w").write("\n".join(summary))
idc.qexit(0)
