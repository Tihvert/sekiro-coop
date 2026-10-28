import os, ida_funcs, ida_hexrays, idautils, idc, idaapi, ida_auto, ida_segment
OUT = r"F:\Temp\claude\C--Users-<user>-Documents-Default-Project-projects-sekiro-seamless\e14bfe0d-afb8-4abb-a78e-0870380b3209\scratchpad\ida\out2"
os.makedirs(OUT, exist_ok=True)
ida_auto.auto_wait()
ida_hexrays.init_hexrays_plugin()
L = []
done = set()
def callees(fea):
    f = ida_funcs.get_func(fea); out = []
    if not f: return out
    for h in idautils.FuncItems(f.start_ea):
        if idaapi.is_call_insn(h):
            t = idc.get_operand_value(h, 0)
            s = ida_segment.getseg(t) if t else None
            if s and (s.perm & ida_segment.SEGPERM_EXEC) and t not in out: out.append(t)
    return out
def dec(ea, depth):
    f = ida_funcs.get_func(ea)
    if not f: L.append(f"{ea:#x} nofunc"); return
    ea = f.start_ea
    if ea in done: return
    done.add(ea)
    try: t = str(ida_hexrays.decompile(ea))
    except Exception as ex: t = f"// failed {ex}"
    open(os.path.join(OUT, f"f_{ea:x}.c"), "w", encoding="utf-8").write(t)
    cs = callees(ea)
    callers = sorted({ida_funcs.get_func(x).start_ea for x in idautils.CodeRefsTo(ea, 0) if ida_funcs.get_func(x)})
    L.append(f"dec {ea:#x} len {len(t)} callees {[hex(c) for c in cs]} callers {[hex(c) for c in callers[:15]]}")
    if depth > 0:
        for c in cs:
            if len(t) < 6000: dec(c, depth - 1)
for ea, d in [(0x140A34B70, 2), (0x140A324E0, 1), (0x140A23730, 1), (0x140A2CD50, 0), (0x140B53170, 0), (0x140960310, 0), (0x140960320, 0), (0x14066AF40, 0), (0x1409EDA50, 0), (0x1411D44F0, 0)]:
    dec(ea, d)
# data refs to the clamp constant
for x in idautils.DataRefsTo(0x143B010A0):
    f = ida_funcs.get_func(x)
    L.append(f"ref 143B010A0 from {x:#x} fn {f.start_ea if f else 0:#x}")
L.append(f"dword_143B010A0 = {idc.get_wide_dword(0x143B010A0):#x}")
open(os.path.join(OUT, "summary.txt"), "w").write("\n".join(L))
idc.qexit(0)
