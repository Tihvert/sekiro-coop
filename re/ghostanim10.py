# IDAPython (headless): ghost animation part 2, pass 5 -> F:\SekiroRE\out35
# Registration of the per-event notify callback 0xB42390 (lea at 0x141042769) and where it is invoked.
import os, ida_bytes, ida_funcs, ida_hexrays, ida_name, ida_segment, idautils, idc

OUT_DIR = r"F:\SekiroRE\out35"
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


def disasm(ea, n):
    h = ea
    for _ in range(n):
        summary.append(f"  {h:#x} {idc.generate_disasm_line(h, 0)}")
        h = idc.next_head(h, h + 0x40)


dec(B + 0x1042480, "reg", 0, 1)
summary.append("DISASM around lea:")
disasm(B + 0x1042740, 30)
callers(B + 0x1042480, "reg_caller", 8, 0)

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
idc.qexit(0)
