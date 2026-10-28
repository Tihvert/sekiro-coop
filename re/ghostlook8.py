# IDAPython (headless): ghost appearance, pass 8 -> F:\SekiroRE\out28
import os, re, ida_bytes, ida_funcs, ida_hexrays, ida_name, ida_pro, ida_segment, idautils, idc

OUT_DIR = r"F:\SekiroRE\out28"
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


for ea, l in [(0x140F14500, "gx_216"), (0x140F144F0, "gx_384"), (0x140F14520, "gx_220"), (0x140F14580, "gx_221"),
              (0x140F144E0, "gx_222"), (0x140F182A0, "gx_152"), (0x140F18260, "gx_184"), (0x140F14570, "gx_280"),
              (0x140F14550, "gx_273"), (0x140F145C0, "gx_272"), (0x140F182E0, "gx_f182e0")]:
    dec(ea, l, 0, 1)
for ea in (0x140F14500, 0x140F144F0, 0x140E841D0 + 0):
    pass
# disasm the +216 flush call site in e841d0
f = 0x140E841D0
fn = ida_funcs.get_func(f)
h = fn.start_ea
while h < fn.end_ea:
    if idc.print_insn_mnem(h) == "call" and idc.get_operand_value(h, 0) in (0x140F14500, 0x140F144F0):
        summary.append(f"CALLSITE {h:#x}:")
        k = h
        for _ in range(6):
            k = idc.prev_head(k, 0)
        for _ in range(8):
            summary.append(f"   {k:#x} {idc.generate_disasm_line(k, 0)}")
            k = idc.next_head(k, fn.end_ea)
    h = idc.next_head(h, fn.end_ea)
with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
