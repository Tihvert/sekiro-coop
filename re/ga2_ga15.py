# pass 11 -> F:\SekiroRE\out42 : accessors of the bm var-index cache (+980h indexed)
import os, ida_funcs, ida_hexrays, idautils, idc
OUT_DIR = r"F:\SekiroRE\out42"; os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
B = 0x140000000
summary, done = [], set()
hits = {}
for fea in idautils.Functions(B + 0x700000, B + 0xC10000):
    f = ida_funcs.get_func(fea)
    h = f.start_ea
    while h < f.end_ea:
        dl = idc.generate_disasm_line(h, 0)
        if "*4+980h]" in dl or "+980h]" in dl and "lea" in dl:
            hits.setdefault(fea, []).append(f"{h:#x} {dl}")
        h = idc.next_head(h, f.end_ea)
for fea, l in hits.items():
    xs = [x.frm for x in idautils.XrefsTo(fea)]
    summary.append(f"USES {fea:#x} callers {len(xs)}: " + " | ".join(l[:4]))
    if len(hits) < 40:
        try:
            t = str(ida_hexrays.decompile(fea))
        except Exception as e:
            t = f"// failed {e}"
        open(os.path.join(OUT_DIR, f"acc_{fea:x}.c"), "w", encoding="utf-8").write(t)
open(os.path.join(OUT_DIR, "summary.txt"), "w").write("\n".join(summary))
idc.qexit(0)
