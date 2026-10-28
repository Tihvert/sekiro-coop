# IDAPython (headless): ghost animation, pass 2 -> F:\SekiroRE\out17
import os, ida_bytes, ida_funcs, ida_hexrays, ida_pro, ida_segment, idautils, idc, ida_ua

OUT_DIR = r"F:\SekiroRE\out17"
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
summary, done = [], set()


def is_code(ea):
    s = ida_segment.getseg(ea)
    return s is not None and (s.perm & ida_segment.SEGPERM_EXEC) != 0


def dec(ea, label, depth=0, maxdepth=0):
    f = ida_funcs.get_func(ea)
    if f is None:
        summary.append(f"{'  ' * depth}{label} {ea:#x} NOFUNC")
        return
    if f.start_ea in done:
        return
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


def funcs_in(lo, hi, label):
    for fea in idautils.Functions(lo, hi):
        dec(fea, label)


def callers(ea, label, n=12):
    out = []
    for x in idautils.XrefsTo(ea):
        g = ida_funcs.get_func(x.frm)
        out.append((x.frm, g.start_ea if g else None))
    summary.append(f"CALLERS of {ea:#x}: {[(hex(a), hex(b) if b else None) for a, b in out[:40]]}")
    for a, b in out[:n]:
        if b:
            dec(b, label)


# A. Module factory vtable and the function that fills the modules table.
FACT_VT = 0x142A8CF98
for i in range(48):
    v = ida_bytes.get_qword(FACT_VT + 8 * i)
    if not is_code(v):
        summary.append(f"fact[{i}] = {v:#x} (end)")
        break
    summary.append(f"fact[{i}] = {v:#x}")
callers(0x140BD9070, "evcreate_caller")      # event module create
callers(0x140BD8C10, "behcreate_caller")     # behavior module create

# B. Who writes ChrIns+0x2118 (what vtbl[247] returns)?
hits = []
ea = 0x140001000
pat = ida_bytes.compiled_binpat_vec_t()
ida_bytes.parse_binpat_str(pat, 0, "18 21 00 00", 16)
while len(hits) < 400:
    ea = ida_bytes.bin_search(ea, 0x14291D000, pat, ida_bytes.BIN_SEARCH_FORWARD)
    if isinstance(ea, tuple):
        ea = ea[0]
    if ea == idc.BADADDR:
        break
    h = idc.get_item_head(ea)
    dis = idc.generate_disasm_line(h, 0) or ""
    if "2118h]" in dis and dis.startswith("mov") and dis.split(",")[0].endswith("2118h]"):
        g = ida_funcs.get_func(h)
        hits.append((h, g.start_ea if g else None, dis))
    ea += 1
summary.append("WRITES +0x2118:")
for h, g, d in hits:
    summary.append(f"  {h:#x} in {(g or 0):#x} : {d}")
for h, g, d in hits[:10]:
    if g:
        dec(g, "w2118")

# C. Event module and TimeAct module method ranges.
funcs_in(0x140BD7E00, 0x140BD8A00, "evr")
funcs_in(0x140B5B000, 0x140B5D400, "tar")
callers(0x140BD84C0, "forcebyev_caller")

# D. Strings mentioning animations, and the functions using them.
for s in idautils.Strings():
    t = str(s)
    if any(k in t for k in ("Animation", "AnimID", "AnimId", "PlayAnim", "ChangeAnim")) and len(t) < 80:
        xr = [x.frm for x in idautils.XrefsTo(s.ea)]
        summary.append(f"STR {s.ea:#x} {t!r} xrefs {[hex(x) for x in xr[:4]]}")

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
