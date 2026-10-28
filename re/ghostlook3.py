# IDAPython (headless): ghost appearance, pass 3 -> F:\SekiroRE\out23
import os, re, ida_bytes, ida_funcs, ida_hexrays, ida_name, ida_pro, ida_segment, idautils, idc

OUT_DIR = r"F:\SekiroRE\out23"
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


for ea, l in [(0x140BF1900, "bf1900"), (0x1406756C0, "d16_user"), (0x1409F5170, "f5170"), (0x1407C4220, "c4220"),
              (0x1408CFFA0, "pdraw_ctor"), (0x1408D09A0, "pdraw_init"), (0x140E66D20, "mdl_ctor"),
              (0x140E69B70, "mdl_e69b70")]:
    dec(ea, l)
for ea, l in [(0x14008FE1D, "lua_getlocalchrtype"), (0x140091F5D, "lua_setchrtypegrey"), (0x1403CE612, "exparam_names"),
              (0x1403D37AF, "dbg_phantom_menu"), (0x1401CD136, "rend_phantom1"), (0x1401CE1E2, "rend_phantom2"),
              (0x1401CE2BC, "rend_phantomlite"), (0x1407C1820, "chrtype_print")]:
    f = func_of(ea)
    if f:
        dec(f, l)
# The lua registration sites: dump nearby instructions to find the function pointer.
for ea in (0x14008FE1D, 0x140091F5D):
    summary.append(f"DISASM around {ea:#x}:")
    h = ea - 0x30
    h = idc.get_item_head(h)
    for _ in range(24):
        summary.append(f"   {h:#x} {idc.generate_disasm_line(h, 0)}")
        h = idc.next_head(h, ea + 0x80)
# PhantomParam table entry
summary.append("QWORDS around 0x143B16500:")
for i in range(-6, 8):
    a = 0x143B16500 + 8 * i
    q = ida_bytes.get_qword(a)
    xs = [hex(x.frm) for x in idautils.XrefsTo(a)][:6]
    summary.append(f"   {a:#x}: {q:#x} xrefs {xs}")
for a in range(0x143B16400, 0x143B16600, 8):
    for x in idautils.XrefsTo(a):
        f = func_of(x.frm)
        summary.append(f"XREF {a:#x} <- {x.frm:#x} in {hex(f) if f else None}")
        if f:
            dec(f, "ptbl_user")
# ExParam tables
for a in (0x143B09998, 0x143B099C0):
    for x in idautils.XrefsTo(a):
        f = func_of(x.frm)
        summary.append(f"XREF {a:#x} <- {x.frm:#x} in {hex(f) if f else None}")
        if f:
            dec(f, "exparam_user")
# EcTestChrType vtable
for ea, name in idautils.Names():
    dn = ida_name.demangle_name(name, 0) or name
    if "vftable" in dn and ("EcTestChrType" in dn or "ChrCmd_GetMatchChrTypeNum" in dn):
        summary.append(f"VT {ea:#x} {dn[:160]}")
        for i in range(8):
            v = ida_bytes.get_qword(ea + 8 * i)
            if is_code(v):
                dec(v, f"ectest{i}")

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
