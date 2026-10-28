# IDAPython (headless): ghost animation, pass 3 -> F:\SekiroRE\out18
import os, ida_bytes, ida_funcs, ida_hexrays, ida_name, ida_pro, ida_segment, idautils, idc

OUT_DIR = r"F:\SekiroRE\out18"
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


def callers(ea, label, n=12, maxdepth=0):
    out = []
    for x in idautils.XrefsTo(ea):
        g = ida_funcs.get_func(x.frm)
        out.append((x.frm, g.start_ea if g else None))
    summary.append(f"CALLERS of {ea:#x}: {[(hex(a), hex(b) if b else None) for a, b in out[:40]]}")
    for a, b in out[:n]:
        if b:
            dec(b, label, 0, maxdepth)
    return out


PLAYER_VT, GHOST_VT = 0x142A2B338, 0x142A2D190

# 1. vtable diff player vs ghost, and where the module builder sits.
summary.append("VT DIFF player vs ghost:")
for i in range(400):
    p = ida_bytes.get_qword(PLAYER_VT + 8 * i)
    g = ida_bytes.get_qword(GHOST_VT + 8 * i)
    if not is_code(p) and not is_code(g):
        summary.append(f"  end at slot {i}")
        break
    if p != g:
        summary.append(f"  [{i}] (+{8*i:#x}) player {p:#x} ghost {g:#x}")
        dec(g, f"ghostov{i:03d}")
    for v, who in ((p, "player"), (g, "ghost")):
        if v in (0x140A002F0,):
            summary.append(f"  module builder at {who} slot {i}")

# 2. Who runs the event module's per-frame apply, and the request checker.
callers(0x140BD81B0, "evupd_caller", 12, 1)
callers(0x140BD8430, "evbusy_caller", 6)
callers(0x140A002F0, "modbuild_ref", 6)

# 3. The callees of the event module apply.
for ea, l in [(0x140FF1DA0, "beh_has_anim"), (0x140B41C40, "beh_get_graph"),
              (0x1409E5C00, "chr18_check"), (0x140BE0930, "mod88_check")]:
    dec(ea, l, 0, 1)

# 4. Ghost second ctor (plain ghost) with its ChrIns base chain, to find ChrIns+0x20.
dec(0x140A39280, "ghostctor", 0, 2)
dec(0x140A493C0, "plainfactory", 0, 1)

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
