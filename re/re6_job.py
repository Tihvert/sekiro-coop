# Generic headless job runner (RE pass "native remote player", 2026-10-22).
# Same commands as re/re3_job.py plus:
#   rtti <regex>        RTTI type names (.?AV...) matching regex -> TD, COLs, vtables (+ slot count)
#   vtn <ea> <n>        vtable slots with function names
#   callers <ea>        code xrefs grouped by function
#   strx <regex>        strings (incl. UTF-16) matching regex + xref functions
# Job file: F:\SekiroRE\scripts6\job.txt
import os, re, struct, ida_funcs, ida_hexrays, idautils, idc, ida_bytes, ida_name, ida_segment, ida_ua, idaapi, ida_nalt
JOB = r"F:\SekiroRE\scripts6\job.txt"
lines = [l.strip() for l in open(JOB, encoding="utf-8") if l.strip() and not l.startswith("#")]
OUT_DIR = lines[0].split(None, 1)[1]
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
BASE = ida_nalt.get_imagebase()
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

_segcache = None
def data_segs():
    global _segcache
    if _segcache is None:
        _segcache = []
        seg = ida_segment.get_first_seg()
        while seg:
            if not (seg.perm & ida_segment.SEGPERM_EXEC):
                b = ida_bytes.get_bytes(seg.start_ea, seg.end_ea - seg.start_ea) or b""
                _segcache.append((seg.start_ea, b))
            seg = ida_segment.get_next_seg(seg.start_ea)
    return _segcache

def rtti(rx):
    rxc = re.compile(rx)
    segs = data_segs()
    tds = []
    for sea, b in segs:
        for m in re.finditer(rb"\.\?A[VU][ -~]{1,200}?@@\x00", b):
            name = m.group(0)[:-1].decode()
            if rxc.search(name):
                td = sea + m.start() - 0x10
                tds.append((td, name))
    for td, name in tds:
        tdr = td - BASE
        pat = struct.pack("<I", tdr & 0xFFFFFFFF)
        cols = []
        for sea, b in segs:
            i = b.find(pat)
            while i >= 0:
                col = sea + i - 0x0C
                if ida_bytes.get_dword(col) == 1:
                    cols.append(col)
                i = b.find(pat, i + 1)
        vts = []
        for col in cols:
            off = ida_bytes.get_dword(col + 4)
            q = struct.pack("<Q", col)
            for sea, b in segs:
                i = b.find(q)
                while i >= 0:
                    vt = sea + i + 8
                    n = 0
                    while n < 600:
                        v = ida_bytes.get_qword(vt + 8 * n)
                        if not is_code(v):
                            break
                        n += 1
                    vts.append((vt, off, n))
                    i = b.find(q, i + 1)
        S.append(f"rtti {name} td {td:#x} " + " ".join(f"vt {v:#x}(off {o:#x}, {n} slots)" for v, o, n in vts))

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
        elif cmd == "rtti":
            rtti(" ".join(a[1:]))
        elif cmd == "xref" or cmd == "callers":
            ea = P(a[1])
            S.append(f"xref {ea:#x}:")
            seen = {}
            for x in idautils.XrefsTo(ea):
                fe = fn_of(x.frm)
                if cmd == "callers":
                    seen.setdefault(fe or 0, []).append(x.frm)
                else:
                    S.append(f"   from {x.frm:#x} fn {fe:#x}" if fe else f"   from {x.frm:#x} (no fn)")
            for fe, fr in seen.items():
                S.append(f"   fn {fe:#x} at {[hex(z) for z in fr[:6]]}")
        elif cmd == "name":
            rx = re.compile(" ".join(a[1:]))
            n = 0
            for ea, nm in idautils.Names():
                dn = ida_name.demangle_name(nm, 0) or nm
                if rx.search(dn):
                    S.append(f"name {ea:#x} {dn}")
                    n += 1
                    if n > 300: break
        elif cmd in ("str", "strx"):
            rx = re.compile(" ".join(a[1:]))
            n = 0
            sl = idautils.Strings()
            if cmd == "strx":
                sl.setup(strtypes=[ida_nalt.STRTYPE_C, ida_nalt.STRTYPE_C_16], minlen=4)
            for s in sl:
                v = str(s)
                if rx.search(v):
                    xs = [f"{x.frm:#x}/{fn_of(x.frm) or 0:#x}" for x in idautils.XrefsTo(s.ea)][:8]
                    S.append(f"str {s.ea:#x} {v!r} xrefs {xs}")
                    n += 1
                    if n > 300: break
        elif cmd in ("vt", "vtn"):
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
