# IDAPython (headless): ghost appearance, pass 6 -> F:\SekiroRE\out26
import os, re, ida_bytes, ida_funcs, ida_hexrays, ida_name, ida_pro, ida_segment, idautils, idc

OUT_DIR = r"F:\SekiroRE\out26"
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


dec(0x140E81EE0, "asmmodel_base_ctor")
dec(0x140B9AF40, "modparam_apply", 0, 1)
dec(0x1409F8520, "f8520")
dec(0x140BFD720, "spfx_phantom_id", 0, 1)
dec(0x1408E4450, "ghostkind_from_data", 0, 1)
dec(0x140A39100, "ghost_ctor_data")
# readers of +0xD8 (216) and +0x180 (384) floats in model code range
for disp, tag in ((0xD8, "r216"), (0x180, "r384"), (0xDC, "r220")):
    seen = {}
    a = 0x140E60000
    while a < 0x140F80000:
        a = idc.next_head(a, 0x140F80000)
        if a == idc.BADADDR:
            break
        if idc.print_insn_mnem(a) in ("movss", "comiss", "ucomiss", "mulss", "movzx", "cmp", "movaps", "movups") :
            for op in (0, 1):
                if idc.get_operand_type(a, op) == idc.o_displ and idc.get_operand_value(a, op) == disp:
                    f = func_of(a)
                    seen.setdefault(f, []).append(a)
    summary.append(f"{tag}: {len(seen)} funcs: {[(hex(k) if k else None, len(v)) for k, v in list(seen.items())[:80]]}")

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
