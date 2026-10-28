# Generic headless job runner (RE pass 3, 2026-10-22).
# Reads F:\SekiroRE\scripts\re3_job.txt:
#   out <dir>                      output directory (first line)
#   dec <ea> [depth]               decompile, recurse into direct callees to depth
#   xref <ea>                      code/data xrefs to ea (with containing function)
#   name <regex>                   names (demangled) matching regex
#   str <regex>                    strings matching regex + xrefs
#   vt <ea> <n>                    vtable slots
#   q <ea> <n>                     qwords
#   b <ea> <n>                     bytes (hex)
#   imm <value> [maxhits]          instructions with that immediate operand
#   disp <value> [maxhits] [mnem]  instructions with that displacement (e.g. 0x1F40)
#   dis <ea> <n>                   disassembly lines
#   fns <lo> <hi>                  decompile every function in [lo, hi)
import os, re, ida_funcs, ida_hexrays, idautils, idc, ida_bytes, ida_name, ida_segment, ida_ua, idaapi
JOB = r"F:\SekiroRE\scripts\re3_job.txt"
lines = [l.strip() for l in open(JOB, encoding="utf-8") if l.strip() and not l.startswith("#")]
OUT_DIR = lines[0].split(None, 1)[1]
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
S = []
done = set()

def fn_of(ea):
    f = ida_funcs.get_func(ea)
    return f.start_ea if f else None

def is_code(ea):
    s = ida_segment.getseg(ea)
    return s is not None and (s.perm & ida_segment.SEGPERM_EXEC) != 0

def callees(fea):
    f = ida_funcs.get_func(fea)
    out = []
    if not f:
        return out
    for h in idautils.FuncItems(f.start_ea):
        if idaapi.is_call_insn(h):
            t = idc.get_operand_value(h, 0)
            if t and is_code(t) and t not in out:
                out.append(t)
    return out

def dec(ea, depth):
    f = ida_funcs.get_func(ea)
    if not f:
        S.append(f"dec {ea:#x}: no function")
        return
    ea = f.start_ea
    if ea in done:
        return
    done.add(ea)
    try:
        t = str(ida_hexrays.decompile(ea))
    except Exception as ex:
        t = f"// failed {ex}"
    open(os.path.join(OUT_DIR, f"f_{ea:x}.c"), "w", encoding="utf-8").write(t)
    cs = callees(ea)
    S.append(f"dec {ea:#x} len {len(t)} callees {[hex(c) for c in cs]}")
    if depth > 0:
        for c in cs:
            dec(c, depth - 1)

def P(x):
    return int(x, 0)

for l in lines[1:]:
    a = l.split()
    cmd = a[0]
    try:
        if cmd == "dec":
            dec(P(a[1]), int(a[2]) if len(a) > 2 else 0)
        elif cmd == "fns":
            lo, hi = P(a[1]), P(a[2])
            for fe in idautils.Functions(lo, hi):
                dec(fe, 0)
        elif cmd == "xref":
            ea = P(a[1])
            S.append(f"xref {ea:#x}:")
            for x in idautils.XrefsTo(ea):
                fe = fn_of(x.frm)
                S.append(f"   from {x.frm:#x} fn {fe:#x}" if fe else f"   from {x.frm:#x} (no fn)")
        elif cmd == "name":
            rx = re.compile(" ".join(a[1:]))
            n = 0
            for ea, nm in idautils.Names():
                dn = ida_name.demangle_name(nm, 0) or nm
                if rx.search(dn):
                    S.append(f"name {ea:#x} {dn}")
                    n += 1
                    if n > 200: break
        elif cmd == "str":
            rx = re.compile(" ".join(a[1:]))
            n = 0
            for s in idautils.Strings():
                v = str(s)
                if rx.search(v):
                    xs = [f"{x.frm:#x}/{fn_of(x.frm) or 0:#x}" for x in idautils.XrefsTo(s.ea)][:8]
                    S.append(f"str {s.ea:#x} {v!r} xrefs {xs}")
                    n += 1
                    if n > 200: break
        elif cmd == "vt":
            ea, n = P(a[1]), int(a[2])
            for i in range(n):
                v = ida_bytes.get_qword(ea + 8 * i)
                S.append(f"vt {ea:#x}[{i}] = {v:#x}")
        elif cmd == "q":
            ea, n = P(a[1]), int(a[2])
            S.append(f"q {ea:#x}: " + " ".join(f"{ida_bytes.get_qword(ea+8*i):#x}" for i in range(n)))
        elif cmd == "b":
            ea, n = P(a[1]), int(a[2])
            S.append(f"b {ea:#x}: " + ida_bytes.get_bytes(ea, n).hex(" "))
        elif cmd == "dis":
            ea, n = P(a[1]), int(a[2])
            for i in range(n):
                S.append(f"dis {ea:#x}: {idc.generate_disasm_line(ea, 0)}")
                ea = idc.next_head(ea, idc.BADADDR)
        elif cmd in ("imm", "disp"):
            val = P(a[1]); mx = int(a[2]) if len(a) > 2 else 300
            mn = a[3] if len(a) > 3 else None
            hits = 0
            seg = ida_segment.get_first_seg()
            while seg and hits < mx:
                if seg.perm & ida_segment.SEGPERM_EXEC:
                    ea = seg.start_ea
                    while ea < seg.end_ea and hits < mx:
                        insn = ida_ua.insn_t()
                        ln = ida_ua.decode_insn(insn, ea)
                        if ln <= 0:
                            ea += 1; continue
                        for op in insn.ops:
                            if op.type == 0: break
                            ok = (cmd == "imm" and op.type == ida_ua.o_imm and (op.value & 0xFFFFFFFFFFFFFFFF) == val) or \
                                 (cmd == "disp" and op.type in (ida_ua.o_displ, ida_ua.o_phrase) and (op.addr & 0xFFFFFFFF) == val)
                            if ok:
                                d = idc.generate_disasm_line(ea, 0)
                                if mn is None or mn in d:
                                    S.append(f"{cmd} {val:#x} @ {ea:#x} fn {fn_of(ea) or 0:#x}: {d}")
                                    hits += 1
                                break
                        ea += ln
                seg = ida_segment.get_next_seg(seg.start_ea)
    except Exception as ex:
        S.append(f"ERR {l}: {ex}")

open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8").write("\n".join(S))
idc.qexit(0)
