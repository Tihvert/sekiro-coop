# RE-FINDINGS-2: flag network-sync predicate (sub_1406C4240) and SetEventFlagAndSync callers
import os, ida_funcs, ida_hexrays, ida_pro, idautils, idc
OUT_DIR = r"F:\SekiroRE\outRE2\sync"
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
BASE = 0x140000000
out = []
def dec(ea, label):
    try: t = str(ida_hexrays.decompile(ea))
    except Exception as e: t = f"// failed {e}"
    out.append(f"// ===== {label} rva {ea-BASE:#x} xrefs {len(list(idautils.CodeRefsTo(ea,0)))}\n{t}")
dec(BASE + 0x6C4240, "IsFlagNetSynced")
for x in idautils.CodeRefsTo(BASE + 0x6C4240, 0):
    f = ida_funcs.get_func(x)
    out.append(f"caller of 6c4240: {f.start_ea-BASE:#x}" if f else "?")
for x in idautils.CodeRefsTo(BASE + 0x6C4890, 0):
    f = ida_funcs.get_func(x)
    if f: dec(f.start_ea, "SetFlagAndSync_caller")
for x in idautils.CodeRefsTo(BASE + 0x8D9ED0, 0):
    f = ida_funcs.get_func(x)
    out.append(f"caller of 8d9ed0: {f.start_ea-BASE:#x}" if f else "?")
for x in idautils.CodeRefsTo(BASE + 0x68E5B0, 0):
    f = ida_funcs.get_func(x); out.append(f"caller of 68e5b0: {x-BASE:#x} in {f.start_ea-BASE if f else 0:#x}")
for x in idautils.DataRefsTo(BASE + 0x68E5B0):
    out.append(f"dataref of 68e5b0: {x-BASE:#x}")
for a in (0x68E5D0, 0x68E5E0):
    for x in list(idautils.CodeRefsTo(BASE + a, 0)) + list(idautils.DataRefsTo(BASE + a)):
        f = ida_funcs.get_func(x); out.append(f"ref of {a:#x}: {x-BASE:#x} in {(f.start_ea-BASE) if f else 0:#x}")
        if f: dec(f.start_ea, f"ref_{a:x}")
open(os.path.join(OUT_DIR, "sync.c"), "w", encoding="utf-8").write("\n".join(out))
ida_pro.qexit(0)
