# IDAPython (headless): ghost animation part 2, pass 2 -> F:\SekiroRE\out31
# Who fills behaviorModule+0x60 (sync recorder R): event list, set-variable list; hks hkbFireEvent binding.
import os, ida_bytes, ida_funcs, ida_hexrays, ida_name, ida_segment, idautils, idc, ida_nalt

OUT_DIR = r"F:\SekiroRE\out31"
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


def callers_of(ea):
    out = []
    for x in idautils.XrefsTo(ea):
        g = ida_funcs.get_func(x.frm)
        out.append(g.start_ea if g else x.frm)
    return out


# 1. Every function in the R class region, with callers.
ea = B + 0xFEA000
while ea < B + 0xFEE400:
    f = ida_funcs.get_func(ea)
    if f is None:
        ea = idc.next_head(ea, B + 0xFEE400)
        continue
    cs = callers_of(f.start_ea)
    summary.append(f"FN {f.start_ea:#x} size {f.end_ea - f.start_ea:#x} callers {len(cs)}: {[hex(c) for c in cs[:10]]}")
    dec(f.start_ea, "rcls")
    ea = f.end_ea

# 2. hks binding table function (string refs hkbFireEvent / hkbSetVariable).
for fe in (0x13F78C0, 0x13F7670):
    f = ida_funcs.get_func(B + fe)
    summary.append(f"DISASM {B + fe:#x}")
    h = f.start_ea
    while h < f.end_ea:
        summary.append(f"  {h:#x} {idc.generate_disasm_line(h, 0)}")
        h = idc.next_head(h, f.end_ea)

# 3. Behavior module ctor and its R init.
for e, l, d in [(0xBD8C10, "bmnew", 1), (0xB44630, "bhvupd", 1), (0xFE9240, "syncname", 0), (0x10158F0, "evname2id", 1),
                (0x10159E0, "varname2id", 1), (0xFF3A30, "varset", 1), (0xFFE740, "varhandle", 0), (0xFF3CC0, "vartype", 0)]:
    dec(B + e, l, 0, d)

# 4. RTTI names containing BehaviorScript / HkBeh / SyncEvent.
for e, name in idautils.Names():
    dn = ida_name.demangle_name(name, 0) or name
    if ("BehaviorScript" in dn or "HkBehSync" in dn or "BehaviorSync" in dn or "SyncEvent" in dn) and "vftable" in dn:
        summary.append(f"VT {e:#x} {dn}")
        for i in range(8):
            v = ida_bytes.get_qword(e + 8 * i)
            if not is_code(v):
                break
            summary.append(f"   [{i}] {v:#x}")

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
idc.qexit(0)
