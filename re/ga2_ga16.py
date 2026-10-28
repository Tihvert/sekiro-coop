# pass 12 -> F:\SekiroRE\out43 : chr predicates used by the locomotion var writer
import os, ida_funcs, ida_hexrays, idautils, idc, ida_bytes
OUT_DIR = r"F:\SekiroRE\out43"; os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
B = 0x140000000
summary = []
for e, l in [(0x9F36C0, "is_x"), (0x9F3430, "is_y"), (0x9EFEA0, "moveinput"), (0x9F0D70, "x9f0d70"), (0xB4C600, "turnangle"),
             (0xB4A970, "b4a970"), (0xB4A270, "b4a270"), (0xB4AE00, "b4ae00"), (0xB4A7F0, "b4a7f0"), (0x1413779F0, "gen")]:
    ea = B + e if e < 0x100000000 else e
    f = ida_funcs.get_func(ea)
    try: t = str(ida_hexrays.decompile(f.start_ea))
    except Exception as ex: t = f"// failed {ex}"
    open(os.path.join(OUT_DIR, f"{l}_{f.start_ea:x}.c"), "w", encoding="utf-8").write(t)
    summary.append(f"{l} {f.start_ea:#x} len {len(t)}")
open(os.path.join(OUT_DIR, "summary.txt"), "w").write("\n".join(summary))
idc.qexit(0)
