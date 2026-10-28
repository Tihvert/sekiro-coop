# IDAPython (headless): ghost animation, pass 4 -> F:\SekiroRE\out19
import os, ida_bytes, ida_funcs, ida_hexrays, ida_pro, ida_segment, idautils, idc

OUT_DIR = r"F:\SekiroRE\out19"
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


def wstr(ea, n=64):
    if not (0x140000000 <= ea < 0x150000000):
        return "<not a pointer>"
    out = []
    for i in range(n):
        c = ida_bytes.get_word(ea + 2 * i)
        if c == 0:
            break
        out.append(chr(c))
    return "".join(out)


# 1. Prefix of the "%s%04d" behavior event name.
p = ida_bytes.get_qword(0x143B151F0)
summary.append(f"PREFIX ptr {p:#x} = {wstr(p)!r}")
for k in range(1, 8):
    q = ida_bytes.get_qword(0x143B151F0 + 8 * k)
    summary.append(f"  table[{k}] {q:#x} = {wstr(q)!r}")
callers(0x140FF0B70, "evname_caller", 8)

# 2. Is the chr update with the event-module apply run for every ChrIns?
callers(0x140A04850, "chrupd_caller", 6)

# 3. The event-entity adapter created at chr+0x2118.
dec(0x140751180, "event_adapter_new", 0, 2)

# 4. Raise-hkb-event and TimeAct-play callers.
callers(0x140FF1D10, "raise_caller", 0)
callers(0x140B5C580, "taplay_caller", 6, 0)

# 5. BehaviorSync module methods and hkb variable apply blocks.
for fea in idautils.Functions(0x140B4E000, 0x140B4FA00):
    dec(fea, "bs")
for fea in idautils.Functions(0x140FF8400, 0x140FF8A00):
    dec(fea, "hkvar")
dec(0x140FEC040, "bs9728", 0, 1)

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
