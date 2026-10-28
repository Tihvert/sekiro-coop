# IDAPython (headless): ghost animation part 2, pass 4 -> F:\SekiroRE\out33
# Who calls the per-event notify 0xB42390 (fn pointer); hks hkbFireEvent binding; recorder frame timing.
import os, ida_bytes, ida_funcs, ida_hexrays, ida_name, ida_segment, idautils, idc, ida_search

OUT_DIR = r"F:\SekiroRE\out33"
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


def callers(ea, label, n=12, maxdepth=0):
    out = []
    for x in idautils.XrefsTo(ea):
        g = ida_funcs.get_func(x.frm)
        out.append((x.frm, g.start_ea if g else None))
    summary.append(f"CALLERS of {ea:#x} ({len(out)}): {[(hex(a), hex(b) if b else None) for a, b in out[:60]]}")
    for a, b in out[:n]:
        if b:
            dec(b, label, 0, maxdepth)
    return out


def find_qword(val, label):
    pat = " ".join(f"{(val >> (8 * i)) & 0xFF:02X}" for i in range(8))
    hits = []
    for seg_ea in idautils.Segments():
        s = ida_segment.getseg(seg_ea)
        ea = s.start_ea
        while True:
            ea = ida_bytes.find_bytes(pat, ea, s.end_ea)
            if ea == idc.BADADDR or ea is None or len(hits) > 20:
                break
            hits.append(ea)
            ea += 1
    summary.append(f"QWORD {label} {val:#x} at {[hex(h) for h in hits]}")
    for h in hits:
        for x in idautils.XrefsTo(h):
            g = ida_funcs.get_func(x.frm)
            summary.append(f"   ref {h:#x} from {x.frm:#x} fn {(g.start_ea if g else 0):#x}")
            if g:
                dec(g.start_ea, label + "_user", 0, 0)
        # also show neighbourhood names (vtable?)
        for k in range(-4, 5):
            q = ida_bytes.get_qword(h + 8 * k)
            summary.append(f"     [{k:+d}] {q:#x} {ida_name.get_name(h + 8 * k)}")
    return hits


for v, l in [(B + 0xB42390, "notify"), (B + 0xB44590, "bm_onevent")]:
    find_qword(v, l)

for e, l, d in [(0x13F92C0, "hks_fire", 1), (0xB42390, "notify", 0), (0x7DEFA0, "manip_pre", 0), (0xB4F440, "bs_pre", 0),
                (0x7EEB50, "capcaller", 0), (0xB49270, "b49270", 0), (0xFF3A30, "varset", 1), (0xFF3570, "varset2", 1),
                (0xFFE740, "varhandle", 1), (0xFF36D0, "getf", 0), (0x1009850, "x1009850", 0), (0xFF0DF0, "name2idx", 0)]:
    dec(B + e, l, 0, d)
callers(B + 0x7DEFA0, "manip_pre_caller", 6, 0)
callers(B + 0x7EEB50, "capcaller_caller", 6, 0)
callers(B + 0xA3BC90, "recorder_caller", 6, 0)
callers(B + 0xB45440, "b45440_c2", 0, 0)

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
idc.qexit(0)
