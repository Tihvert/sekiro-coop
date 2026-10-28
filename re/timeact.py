import os, ida_funcs, ida_hexrays, ida_name, ida_pro, idautils, idc
OUT_DIR = r"F:\SekiroRE\out15"
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
TEXT_LO, TEXT_HI = 0x140001000, 0x14291D000
summary=[]
for ea, name in idautils.Names():
    dn = ida_name.demangle_name(name, 0) or name
    if ("SprjChrTimeActModule::`vftable" in dn) or ("SprjChrBehaviorSyncModule::`vftable" in dn) or ("SprjChrEventModule::`vftable" in dn):
        tag = "ta" if "TimeAct" in dn else ("bs" if "Sync" in dn else "ev")
        summary.append(f"VFTABLE {ea:#x} {dn}")
        for i in range(40):
            v = idc.get_qword(ea + 8*i)
            if not (TEXT_LO <= v < TEXT_HI): break
            f = ida_funcs.get_func(v)
            if f is None: continue
            try: text = str(ida_hexrays.decompile(f.start_ea))
            except Exception as e: text = f"// failed {e}"
            open(os.path.join(OUT_DIR, f"{tag}{i:02d}_{v:x}.c"), "w", encoding="utf-8").write(text)
            summary.append(f"  {tag}[{i}] {v:#x} len {len(text)}")
open(os.path.join(OUT_DIR, "summary.txt"), "w").write("\n".join(summary))
ida_pro.qexit(0)
