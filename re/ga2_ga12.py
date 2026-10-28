# pass 7 -> F:\SekiroRE\out37 : callback roles, prologues, delegate object
import os, ida_bytes, ida_funcs, ida_hexrays, ida_segment, idautils, idc
OUT_DIR = r"F:\SekiroRE\out37"; os.makedirs(OUT_DIR, exist_ok=True)
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
def prologue(ea, n=8):
    summary.append(f"PROLOGUE {ea:#x}:")
    h = ea
    for _ in range(n):
        size = idc.get_item_size(h)
        b = " ".join(f"{ida_bytes.get_byte(h + i):02X}" for i in range(size))
        summary.append(f"  +{h - ea:02X} {b:<32} {idc.generate_disasm_line(h, 0)}")
        h = idc.next_head(h, ea + 0x100)
for e, l in [(0xB42430, "cb_b42430"), (0xB42230, "cb_b42230"), (0xB41BF0, "cb_b41bf0"), (0xB4E4E0, "cb_b4e4e0"),
             (0xB4E560, "cb_b4e560"), (0xB4E520, "cb_b4e520"), (0xB4E590, "cb_b4e590"), (0x100CD00, "inv_100cd00"),
             (0x10141D0, "inv_10141d0"), (0x10149B0, "inv_10149b0"), (0xB2BBD0, "cb_b2bbd0"), (0xB5C170, "cb_b5c170"),
             (0xB4EE10, "cb_b4ee10"), (0x1042CE0, "cb_1042ce0"), (0xFF3570, "varset2"), (0xFF1F10, "x"), (0xB44630, "bhvupd")]:
    dec(B + e, l)
for e in (0xB42390, 0xB44590, 0xFED450, 0x8D7000, 0x8DCF90, 0xB4EFE0, 0xFEC790, 0xFF3A30, 0x1001270):
    prologue(B + e)
# Find all writers of the delegate: who does "call qword ptr [rax+50h]" right after loading [rcx+20h] of singleton? skip.
# Functions in 0x14100xxxx-0x141017xxxx that load from [reg+20h] then call through vtable +0x50 / +0x58 etc.
open(os.path.join(OUT_DIR, "summary.txt"), "w").write("\n".join(summary))
idc.qexit(0)
