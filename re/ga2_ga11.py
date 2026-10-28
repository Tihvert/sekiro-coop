# pass 6 -> F:\SekiroRE\out36 : disasm notify setter chain, find invocation of the stored callback
import os, ida_bytes, ida_funcs, ida_hexrays, ida_segment, idautils, idc
OUT_DIR = r"F:\SekiroRE\out36"; os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
B = 0x140000000
summary, done = [], set()
def dec(ea, label):
    f = ida_funcs.get_func(ea)
    if f is None or f.start_ea in done: return
    done.add(f.start_ea)
    try: text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e: text = f"// failed: {e}"
    open(os.path.join(OUT_DIR, f"{label}_{f.start_ea:x}.c"), "w", encoding="utf-8").write(f"// {label}\n{text}")
    summary.append(f"{label} {f.start_ea:#x} len {len(text)}")
def disasm_fn(ea, label, n=60):
    f = ida_funcs.get_func(ea)
    summary.append(f"DISASM {label} {ea:#x}")
    h = ea; end = f.end_ea if f else ea + 0x80
    k = 0
    while h < end and k < n:
        summary.append(f"  {h:#x} {idc.generate_disasm_line(h, 0)}"); h = idc.next_head(h, end); k += 1
for e, l in [(0x1015E60, "set_notify"), (0x1015E10, "set_b42430"), (0x100CC40, "set_inner"), (0x1015EA0, "set_tae")]:
    disasm_fn(B + e, l, 40)
# Who reads qword_143D92828 + small offsets with an indirect call nearby: list all functions referencing the singleton
refs = set()
for x in idautils.XrefsTo(B + 0x3D92828):
    f = ida_funcs.get_func(x.frm)
    if f: refs.add(f.start_ea)
summary.append(f"SINGLETON 0x143d92828 users {len(refs)}: {[hex(r) for r in sorted(refs)]}")
refs2 = set()
for x in idautils.XrefsTo(B + 0x3D91E20):
    f = ida_funcs.get_func(x.frm)
    if f: refs2.add(f.start_ea)
summary.append(f"SINGLETON 0x143d91e20 users {len(refs2)}: {[hex(r) for r in sorted(refs2)]}")
open(os.path.join(OUT_DIR, "summary.txt"), "w").write("\n".join(summary))
idc.qexit(0)
