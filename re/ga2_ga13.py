# pass 8 -> F:\SekiroRE\out38 : find invokers of the delegate at singleton(0x143D92828)+0x20
import os, ida_bytes, ida_funcs, ida_hexrays, idautils, idc
OUT_DIR = r"F:\SekiroRE\out38"; os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
B = 0x140000000
summary = []
for x in idautils.XrefsTo(B + 0x3D92828):
    h = x.frm
    seq = []
    for _ in range(10):
        seq.append(f"{h:#x} {idc.generate_disasm_line(h, 0)}")
        h = idc.next_head(h, h + 0x40)
    if any("+20h]" in s for s in seq[1:5]):
        f = ida_funcs.get_func(x.frm)
        summary.append(f"USER {x.frm:#x} fn {(f.start_ea if f else 0):#x}")
        summary.extend("   " + s for s in seq)
        if f:
            try:
                t = str(ida_hexrays.decompile(f.start_ea))
                open(os.path.join(OUT_DIR, f"inv_{f.start_ea:x}.c"), "w", encoding="utf-8").write(t)
            except Exception:
                pass
open(os.path.join(OUT_DIR, "summary.txt"), "w").write("\n".join(summary))
idc.qexit(0)
