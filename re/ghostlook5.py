# IDAPython (headless): ghost appearance, pass 5 -> F:\SekiroRE\out25
import os, re, ida_bytes, ida_funcs, ida_hexrays, ida_name, ida_pro, ida_segment, idautils, idc

OUT_DIR = r"F:\SekiroRE\out25"
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


# PlayerCtrl vtable slot 7
for ea, name in idautils.Names():
    dn = ida_name.demangle_name(name, 0) or name
    if dn.endswith("NS_SPRJ::PlayerCtrl::`vftable'") or dn.endswith("NS_SPRJ::ChrCtrl::`vftable'"):
        v = ida_bytes.get_qword(ea + 56)
        summary.append(f"VT {ea:#x} {dn} [7]={v:#x}")
        dec(v, "ctrl_vf7_" + ("pl" if "Player" in dn else "chr"), 0, 1)
# Globals
for a in (0x143D88880, 0x143D88890, 0x143D88840):
    fs = [ida_bytes.get_dword(a + 4 * i) for i in range(4)]
    summary.append(f"XMM {a:#x}: {[hex(f) for f in fs]}")
# readers of the look object: mov reg,[reg+120h] followed by a dword load from [reg]
d = (0x120).to_bytes(4, "little")
seen = set()
for h in scan_bytes(re.escape(d)):
    head = idc.get_item_head(h)
    if idc.print_insn_mnem(head) != "mov" or "+120h]" not in (idc.print_operand(head, 1) or ""):
        continue
    dst = idc.print_operand(head, 0)
    n = idc.next_head(head, head + 0x40)
    t1 = idc.print_operand(n, 1) or ""
    t0 = idc.print_operand(n, 0) or ""
    if t1 == f"[{dst}]" or t0 == f"[{dst}]" or t1 == f"dword ptr [{dst}]" or t0 == f"dword ptr [{dst}]":
        f = func_of(head)
        if f and f not in seen:
            seen.add(f)
            summary.append(f"LOOKREAD {head:#x} in {f:#x}: {idc.generate_disasm_line(head, 0)} / {idc.generate_disasm_line(n, 0)}")
summary.append(f"LOOKREAD funcs {len(seen)}")
for f in list(seen)[:40]:
    dec(f, "lookrd")
# disp scan for chr colour block +0x1A68..
for disp in (0x1A68, 0x1A78, 0x1A88, 0x1A98):
    d = disp.to_bytes(4, "little")
    fl = []
    for h in scan_bytes(re.escape(d)):
        head = idc.get_item_head(h)
        txt = idc.generate_disasm_line(head, 0) or ""
        if f"{disp:X}H" in txt.upper():
            fl.append((head, func_of(head), txt))
    summary.append(f"DISP {disp:#x} {len(fl)}")
    for a, f, t in fl[:60]:
        summary.append(f"   {a:#x} in {hex(f) if f else None}: {t}")
    for a, f, t in fl[:25]:
        if f:
            dec(f, f"c{disp:x}")

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
