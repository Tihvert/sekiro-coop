# IDAPython (headless): ghost animation, pass 5 -> F:\SekiroRE\out20
import os, ida_bytes, ida_funcs, ida_hexrays, ida_name, ida_pro, ida_segment, idautils, idc

OUT_DIR = r"F:\SekiroRE\out20"
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
    summary.append(f"CALLERS of {ea:#x} ({len(out)}): {[(hex(a), hex(b) if b else None) for a, b in out[:60]]}")
    for a, b in out[:n]:
        if b:
            dec(b, label, 0, maxdepth)
    return out


def prologue(ea, n=8):
    summary.append(f"PROLOGUE {ea:#x}:")
    h = ea
    for _ in range(n):
        size = idc.get_item_size(h)
        b = " ".join(f"{ida_bytes.get_byte(h + i):02X}" for i in range(size))
        summary.append(f"  +{h - ea:02X} {b:<32} {idc.generate_disasm_line(h, 0)}")
        h = idc.next_head(h, ea + 0x100)


# 1. EventChrEntity vtable.
for ea, name in idautils.Names():
    dn = ida_name.demangle_name(name, 0) or name
    if "EventChrEntity::`vftable" in dn:
        summary.append(f"VT {ea:#x} {dn}")
        for i in range(12):
            v = ida_bytes.get_qword(ea + 8 * i)
            summary.append(f"  [{i}] {v:#x}")
            if is_code(v):
                dec(v, f"evchr{i:02d}")

# 2. hkb event raisers.
for ea, l in [(0x140FF1CD0, "raise_wrap"), (0x141001270, "raise_1001270"), (0x140AE90B0, "raise_ae90b0"),
              (0x140FF20D0, "evq_grow")]:
    dec(ea, l)
callers(0x140FF1CD0, "raise_wrap_caller", 0)
callers(0x140FF1DA0, "wevent_caller", 0)

# 3. Prologues for hook/call.
for ea in (0x140FF1D10, 0x140FF1CD0, 0x140BD84C0, 0x140BD8430, 0x140B5C580):
    prologue(ea)

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
