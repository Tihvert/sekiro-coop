# IDAPython (headless): ghost appearance, pass 1 -> F:\SekiroRE\out21
import os, re, ida_bytes, ida_funcs, ida_hexrays, ida_name, ida_pro, ida_segment, idautils, idc

OUT_DIR = r"F:\SekiroRE\out21"
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


# 1. Small helpers.
for ea, l in [(0x1409EEEC0, "get_chr_type"), (0x140BF1B90, "chrtype_tbl1"), (0x1407E35C0, "setlook_tail"),
              (0x140A16740, "lookobj_ctor"), (0x1407DE430, "vf14_ctrl"), (0x140A2B5B0, "pl_vf248"),
              (0x140A2AE70, "pl_vf249"), (0x140A2B0D0, "pl_vf250"), (0x140A28A80, "pl_vf251"),
              (0x140A34F90, "pl_vf252"), (0x140A38C10, "pl_vf25"), (0x1409F0720, "chr_vf130"),
              (0x1409F3170, "chr_f3170"), (0x1409F34E0, "pl_vf155"), (0x14106D510, "g22a0_ctor"),
              (0x14106D560, "g22a0_get"), (0x140A2C2B0, "pl_vf9"), (0x1407F2810, "ctrl_ctor"),
              (0x1409F6780, "set_look"), (0x140A393A0, "ghost_dtor"), (0x140BC3140, "model_init")]:
    dec(ea, l)

# 2. chr-type property table at 0x143B0A3B0 (20 bytes x 14) and its readers.
tbl = 0x143B0A3B0
summary.append("CHRTYPE TABLE 0x143B0A3B0:")
for i in range(15):
    row = " ".join(f"{ida_bytes.get_byte(tbl + 20 * i + k):02X}" for k in range(20))
    summary.append(f"  [{i:2d}] {row}")
for ea in range(tbl - 0x20, tbl + 20 * 15 + 0x20):
    xs = list(idautils.XrefsTo(ea))
    if xs:
        summary.append(f"XREF to {ea:#x} (+{ea - tbl:#x}): {[hex(x.frm) for x in xs[:40]]}")
        for x in xs[:40]:
            g = ida_funcs.get_func(x.frm)
            if g:
                dec(g.start_ea, f"tbl_{ea - tbl:+x}".replace("+", "p").replace("-", "m"))

# 3. Virtual calls through slots 248..252 (call/jmp [reg+disp32]).
for slot in range(248, 253):
    d = (slot * 8).to_bytes(4, "little")
    pat = rb"\xFF[\x90-\x97\xA0-\xA7]" + re.escape(d)
    hits = scan_bytes(pat)
    fl = []
    for h in hits:
        g = ida_funcs.get_func(h)
        fl.append((hex(h), hex(g.start_ea) if g else None))
    summary.append(f"VCALL slot {slot} (+{slot * 8:#x}) {len(hits)} hits: {fl[:60]}")
    for h, g in fl[:25]:
        if g:
            dec(int(g, 16), f"vc{slot}")

# 4. Instructions using displacement 0x22A0..0x22AD.
for disp in (0x22A0, 0x22A1, 0x22A4, 0x22A8, 0x22AC, 0x22AD, 0x1A3C):
    d = disp.to_bytes(4, "little")
    hits = scan_bytes(re.escape(d))
    fl = []
    for h in hits:
        head = idc.get_item_head(h)
        txt = idc.generate_disasm_line(head, 0) or ""
        if f"{disp:X}h" not in txt.upper().replace("0X", ""):
            continue
        g = ida_funcs.get_func(head)
        fl.append((hex(head), hex(g.start_ea) if g else None, txt))
    summary.append(f"DISP {disp:#x} {len(fl)} hits:")
    for h, g, t in fl[:80]:
        summary.append(f"   {h} in {g}: {t}")
    for h, g, t in fl[:30]:
        if g:
            dec(int(g, 16), f"d{disp:x}")

# 5. Strings.
pats = ["phantom", "transparen", "ghost", "chrtype", "teamtype", "alpha", "fade", "translucen", "phantomparam"]
for s in idautils.Strings():
    try:
        v = str(s)
    except Exception:  # noqa: BLE001
        continue
    lv = v.lower()
    if any(p in lv for p in pats) and len(v) < 200:
        xs = [hex(x.frm) for x in idautils.XrefsTo(s.ea)][:8]
        summary.append(f"STR {s.ea:#x} {v!r} xrefs {xs}")

# 6. RTTI names of interest.
for ea, name in idautils.Names():
    dn = ida_name.demangle_name(name, 0) or name
    l = dn.lower()
    if "vftable" in l and any(p in l for p in ("phantom", "ghost", "transp", "drawparam", "chrmodel", "fade", "silhouette")):
        summary.append(f"VT {ea:#x} {dn}")

# 7. callers of GetChrType (list only) and of chrtype_tbl1.
callers(0x1409EEEC0, "gct_caller", 0)
callers(0x140BF1B90, "tbl1_caller", 0)

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
