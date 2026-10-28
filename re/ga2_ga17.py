# pass 13 -> F:\SekiroRE\out44 : ReplayGhostIns / PlayerIns vtable slots 53/54 (is-remote predicate)
import os, ida_funcs, ida_hexrays, idautils, idc, ida_bytes, ida_name
OUT_DIR = r"F:\SekiroRE\out44"; os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
summary, done = [], set()
for ea, name in idautils.Names():
    dn = ida_name.demangle_name(name, 0) or name
    if dn in ("const NS_SPRJ::ReplayGhostIns::`vftable'", "const NS_SPRJ::PlayerIns::`vftable'", "const NS_SPRJ::ChrIns::`vftable'",
              "const NS_SPRJ::EnemyIns::`vftable'"):
        for slot in (53, 54, 247):
            v = ida_bytes.get_qword(ea + 8 * slot)
            summary.append(f"{dn} {ea:#x} slot {slot} -> {v:#x}")
            f = ida_funcs.get_func(v)
            if f and f.start_ea not in done:
                done.add(f.start_ea)
                try: t = str(ida_hexrays.decompile(f.start_ea))
                except Exception as ex: t = f"// failed {ex}"
                open(os.path.join(OUT_DIR, f"slot{slot}_{f.start_ea:x}.c"), "w", encoding="utf-8").write(t)
open(os.path.join(OUT_DIR, "summary.txt"), "w").write("\n".join(summary))
idc.qexit(0)
