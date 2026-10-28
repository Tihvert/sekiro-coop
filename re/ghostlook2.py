# IDAPython (headless): ghost appearance, pass 2 -> F:\SekiroRE\out22
import os, re, ida_bytes, ida_funcs, ida_hexrays, ida_name, ida_pro, ida_segment, idautils, idc

OUT_DIR = r"F:\SekiroRE\out22"
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



for ea, l in [(0x140E7B000, "model_create"), (0x140BF1A20, "tbl_bf1a20"), (0x1409F05F0, "f05f0"),
              (0x1409F3430, "f3430"), (0x140E69B70, "mdl_e69b70"), (0x140A361C0, "build_model"),
              (0x140A2F780, "draw_upd"), (0x140E6C2B0, "mdl_e6c2b0"), (0x140A29030, "f29030"), (0x140A291F0, "f291f0"),
              (0x1409F6B20, "f6b20"), (0x140A16740, "lookobj_ctor")]:
    dec(ea, l)
callers(0x140A361C0, "build_model_caller", 10)
for a, l in [(0x140BF18A0, "b6"), (0x140BF18D0, "d16"), (0x140BF19F0, "d12"), (0x140BF1A50, "b4"), (0x140BF1A80, "b7"),
             (0x140BF1AB0, "b8"), (0x140BF1AE0, "b0"), (0x140BF1B00, "b3"), (0x140BF1B30, "b2"), (0x140BF1B90, "b1"),
             (0x140BF1A20, "bf1a20")]:
    callers(a, "acc_" + l, 40)
for ea in (0x143B06608,):
    summary.append(f"DWORD {ea:#x} = {ida_bytes.get_dword(ea):#x}")
for ea, name in idautils.Names():
    dn = ida_name.demangle_name(name, 0) or name
    if "vftable" in dn and any(k in dn for k in ("ModelIns", "ChrModel", "DrawParam", "ChrDraw", "CSChrModel")):
        summary.append(f"VT {ea:#x} {dn[:150]}")
for disp in (0x22A1, 0x22A4, 0x22A8, 0x22AC, 0x2150):
    d = disp.to_bytes(4, "little")
    hits = scan_bytes(re.escape(d))
    fl = []
    for h in hits:
        head = idc.get_item_head(h)
        txt = idc.generate_disasm_line(head, 0) or ""
        if f"{disp:X}H" not in txt.upper():
            continue
        g = ida_funcs.get_func(head)
        fl.append((hex(head), hex(g.start_ea) if g else None, txt))
    summary.append(f"DISP {disp:#x} {len(fl)} hits:")
    for h, g, t in fl[:150]:
        summary.append(f"   {h} in {g}: {t}")
    if disp != 0x2150:
        for h, g, t in fl[:30]:
            if g:
                dec(int(g, 16), f"d{disp:x}")
for w in ("Phantom", "PHANTOM", "ChrType", "Transparen"):
    pat = re.escape(w.encode("utf-16-le"))
    for s in idautils.Segments():
        seg = ida_segment.getseg(s)
        if seg.perm & ida_segment.SEGPERM_EXEC:
            continue
        data = ida_bytes.get_bytes(seg.start_ea, seg.end_ea - seg.start_ea) or b""
        for m in re.finditer(pat, data):
            j = m.start()
            n = 0
            while j >= 2 and data[j - 2:j] != b"\x00\x00" and n < 40:
                j -= 2
                n += 1
            k = m.end()
            n = 0
            while k + 1 < len(data) and data[k:k + 2] != b"\x00\x00" and n < 60:
                k += 2
                n += 1
            txt = data[j:k].decode("utf-16-le", "replace")
            xs = [hex(x.frm) for x in idautils.XrefsTo(seg.start_ea + j)][:6]
            summary.append(f"WSTR {seg.start_ea + j:#x} {txt!r} xrefs {xs}")

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
