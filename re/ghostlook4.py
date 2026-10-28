# IDAPython (headless): ghost appearance, pass 4 -> F:\SekiroRE\out24
import os, re, ida_bytes, ida_funcs, ida_hexrays, ida_name, ida_pro, ida_segment, idautils, idc

OUT_DIR = r"F:\SekiroRE\out24"
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
summary, done = [], set()
B = 0x140000000


def is_code(ea):
    s = ida_segment.getseg(ea)
    return s is not None and (s.perm & ida_segment.SEGPERM_EXEC) != 0


def dec(ea, label, depth=0, maxdepth=0):
    f = ida_funcs.get_func(ea)
    if f is None:
        summary.append(f"{'  ' * depth}{label} {ea:#x} NOFUNC")
        return None
    if f.start_ea in done:
        return None
    done.add(f.start_ea)
    try:
        text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e:  # noqa: BLE001
        text = f"// failed: {e}"
    with open(os.path.join(OUT_DIR, f"{label}_{f.start_ea:x}.c"), "w", encoding="utf-8") as fh:
        fh.write(f"// {label} {f.start_ea:#x}\n{text}")
    summary.append(f"{'  ' * depth}{label} {f.start_ea:#x} len {len(text)}")
    if depth < maxdepth:
        ea2 = f.start_ea
        while ea2 < f.end_ea:
            if idc.print_insn_mnem(ea2) in ("call", "jmp"):
                t = idc.get_operand_value(ea2, 0)
                if is_code(t) and not (f.start_ea <= t < f.end_ea):
                    dec(t, label + "_c", depth + 1, maxdepth)
            ea2 = idc.next_head(ea2, f.end_ea)
    return text


def callers(ea, label, n=12, maxdepth=0):
    out = []
    for x in idautils.XrefsTo(ea):
        g = ida_funcs.get_func(x.frm)
        out.append((x.frm, g.start_ea if g else None))
    summary.append(f"CALLERS of {ea:#x} ({len(out)}): {[(hex(a), hex(b) if b else None) for a, b in out[:80]]}")
    for a, b in out[:n]:
        if b:
            dec(b, label, 0, maxdepth)
    return out


code_segs = []
for s in idautils.Segments():
    seg = ida_segment.getseg(s)
    if seg.perm & ida_segment.SEGPERM_EXEC:
        code_segs.append((seg.start_ea, seg.end_ea))
summary.append(f"code segs {[(hex(a), hex(b)) for a, b in code_segs]}")


def scan_bytes(pat):
    hits = []
    rx = re.compile(pat, re.S)
    for a, b in code_segs:
        data = ida_bytes.get_bytes(a, b - a)
        if not data:
            continue
        for m in rx.finditer(data):
            hits.append(a + m.start())
    return hits



def func_of(ea):
    g = ida_funcs.get_func(ea)
    return g.start_ea if g else None


dec(0x1409F64D0, "chrtype_refresh", 0, 2)
dec(0x140752370, "emevd_setchrtype", 0, 1)
dec(0x1407A06D0, "pgd_a06d0", 0, 1)
dec(0x140E871A0, "chrmodel_base_ctor")
dec(0x14068D3C0, "lua_setgrey")
dec(0x140689D00, "lua_getlocaltype")
callers(0x1409F5170, "setchrtype_caller", 30)
callers(0x140752370, "emevd_setchrtype_caller", 10)
# Param lookups: calls to 0x1410CE0E0 with the index in edx.
PL = 0x1410CE0E0
idx = {}
for x in idautils.XrefsTo(PL):
    if not idc.print_insn_mnem(x.frm) == "call":
        continue
    h = x.frm
    val = None
    for _ in range(8):
        h = idc.prev_head(h, 0)
        if idc.print_insn_mnem(h) == "mov" and idc.print_operand(h, 0) == "edx" and idc.get_operand_type(h, 1) == idc.o_imm:
            val = idc.get_operand_value(h, 1)
            break
    idx.setdefault(val, []).append(x.frm)
summary.append("PARAM LOOKUP indices: " + ", ".join(f"{k}:{len(v)}" for k, v in sorted(idx.items(), key=lambda t: (t[0] is None, t[0] or 0))))
for k in (57, 58, 59):
    for a in idx.get(k, [])[:20]:
        f = func_of(a)
        summary.append(f"PARAM {k} at {a:#x} in {hex(f) if f else None}")
        if f:
            dec(f, f"param{k}")

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
