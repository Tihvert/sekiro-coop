# pass 10 -> F:\SekiroRE\out41 : who writes the cached locomotion vars (bm+0x980+4k) each frame
import os, ida_funcs, ida_hexrays, idautils, idc, ida_bytes
OUT_DIR = r"F:\SekiroRE\out41"; os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
B = 0x140000000
summary, done = [], set()
def dec(ea, label):
    f = ida_funcs.get_func(ea)
    if f is None or f.start_ea in done: return None
    done.add(f.start_ea)
    try: text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e: text = f"// failed: {e}"
    open(os.path.join(OUT_DIR, f"{label}_{f.start_ea:x}.c"), "w", encoding="utf-8").write(f"// {label}\n{text}")
    summary.append(f"{label} {f.start_ea:#x} len {len(text)}")
    return text
for e, l in [(0xB49D30, "b49d30"), (0xB49950, "b49950"), (0xB41CC0, "b41cc0"), (0xFF4890, "varidx_by_name"),
             (0xB456A0, "b456a0"), (0xB46870, "b46870"), (0xFFED20, "ffed20"), (0xFFEC80, "ffec80"), (0xFFF390, "fff390"),
             (0xB49270, "b49270"), (0xA04850, "chrupd")]:
    dec(B + e, l)
# functions in the behavior module range that access [reg+988h]/[reg+984h]/[reg+98Ch] (MoveAngle/Level/Direction cache)
hits = {}
for fea in idautils.Functions(B + 0xB38000, B + 0xB60000):
    f = ida_funcs.get_func(fea)
    h = f.start_ea
    while h < f.end_ea:
        dl = idc.generate_disasm_line(h, 0)
        for k in ("+984h]", "+988h]", "+98Ch]", "+990h]"):
            if k in dl:
                hits.setdefault(fea, []).append(f"{h:#x} {dl}")
        h = idc.next_head(h, f.end_ea)
for fea, l in hits.items():
    summary.append(f"USES {fea:#x}: " + " | ".join(l[:6]))
    dec(fea, "cacheuser")
open(os.path.join(OUT_DIR, "summary.txt"), "w").write("\n".join(summary))
idc.qexit(0)
