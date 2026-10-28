import os, struct, ida_funcs, ida_hexrays, idautils, idc, ida_bytes, ida_segment, ida_ua, idaapi, ida_nalt, ida_auto
OUT = r"F:\Temp\claude\C--Users-<user>-Documents-Default-Project-projects-sekiro-seamless\e14bfe0d-afb8-4abb-a78e-0870380b3209\scratchpad\ida\out"
os.makedirs(OUT, exist_ok=True)
ida_auto.auto_wait()
ida_hexrays.init_hexrays_plugin()
L = []
DISPS = [0x2128, 0x212C, 0x2130, 0x2134, 0x2138, 0x213C]
def segs():
    for s in idautils.Segments():
        sg = ida_segment.getseg(s)
        if sg.perm & ida_segment.SEGPERM_EXEC:
            yield sg.start_ea, sg.end_ea
hits = {}
for d in DISPS:
    pat = struct.pack("<I", d)
    for a, b in segs():
        ea = a
        while True:
            ea = ida_bytes.find_bytes(pat, ea, range_end=b)
            if ea == idaapi.BADADDR or ea >= b:
                break
            for k in range(2, 10):
                st = ea - k
                insn = ida_ua.insn_t()
                n = ida_ua.decode_insn(insn, st)
                if n and st + n > ea:
                    ok = False
                    for i in range(3):
                        op = insn.ops[i]
                        if op.type in (ida_ua.o_displ, ida_ua.o_phrase) and (op.addr & 0xFFFFFFFF) == d:
                            ok = (i == 0)
                            hits.setdefault(st, (d, idc.generate_disasm_line(st, 0), "W" if i == 0 else "R"))
                    if st in hits:
                        break
            ea += 1
funcs = {}
for st, (d, txt, rw) in sorted(hits.items()):
    f = ida_funcs.get_func(st)
    fs = f.start_ea if f else 0
    L.append(f"{rw} {d:#x} {st:#x} fn {fs:#x} | {txt}")
    if rw == "W" and fs:
        funcs.setdefault(fs, []).append(st)
L.append("")
def dec(ea):
    try:
        t = str(ida_hexrays.decompile(ea))
    except Exception as ex:
        t = f"// failed {ex}"
    open(os.path.join(OUT, f"f_{ea:x}.c"), "w", encoding="utf-8").write(t)
    return len(t)
for fs in funcs:
    n = dec(fs)
    cs = set()
    for x in idautils.CodeRefsTo(fs, 0):
        g = ida_funcs.get_func(x)
        if g: cs.add(g.start_ea)
    L.append(f"writer fn {fs:#x} len {n} callers {[hex(c) for c in sorted(cs)]}")
    for c in sorted(cs)[:12]:
        dec(c)
for extra in [0x1409F3430, 0x1411D44F0, 0x1411D4730, 0x1411D42F0, 0x1411D4C80]:
    L.append(f"extra {extra:#x} len {dec(extra)}")
open(os.path.join(OUT, "summary.txt"), "w").write("\n".join(L))
idc.qexit(0)
