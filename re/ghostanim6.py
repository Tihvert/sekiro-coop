# IDAPython (headless): ghost animation part 2, pass 1 -> F:\SekiroRE\out30
# Sender capture (BehaviorSync 0xB4F1A0 / behaviorModule+0x60), variable blocks, hks bindings.
import os, ida_bytes, ida_funcs, ida_hexrays, ida_name, ida_segment, idautils, idc, ida_nalt

OUT_DIR = r"F:\SekiroRE\out30"
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
    summary.append(f"CALLERS of {ea:#x} ({len(out)}): {[(hex(a), hex(b) if b else None) for a, b in out[:80]]}")
    for a, b in out[:n]:
        if b:
            dec(b, label, 0, maxdepth)
    return out


for ea, l, d in [(0xFED3D0, "evlist", 1), (0xFECE80, "varchk", 1), (0xFEC790, "varfill", 1), (0x8D4800, "frminit", 0),
                 (0x8DBBA0, "frmpack", 1), (0xB42190, "bmflag", 1), (0xFFEBB0, "varapply", 1), (0xFFE020, "pair", 0),
                 (0xFE9240, "syncname", 1), (0xFF0DF0, "name2idx", 1), (0xAE90B0, "ae90b0", 0), (0xB411C0, "b411c0", 1),
                 (0xB4F510, "b4f510", 1), (0xA3A170, "a3a170", 0), (0x7F4C90, "rpapply", 0), (0xFF0BD0, "name2idx2", 0)]:
    dec(B + ea, l, 0, d)

for ea, l in [(0xFF20D0, "qgrow_caller"), (0xFED3D0, "evlist_caller"), (0xFEC790, "varfill_caller"),
              (0xFECE80, "varchk_caller"), (0xB4F1A0, "cap_caller"), (0xB4EFE0, "apply_caller"), (0xFFEBB0, "varapply_caller"),
              (0x1001270, "raiseon_caller"), (0xFF1CD0, "raisename_caller")]:
    callers(B + ea, l, 16, 0)

# Strings
keys = ["hkbFireEvent", "FireEvent", "ExecEvent", "SetVariable", "GetVariable", "hkbSet", "hkbGet", "MoveSpeed", "MoveDirection",
        "MoveAngle", "Locomotion", "MoveType", "TurnSpeed", "LockOn", "SyncEvent", "syncEvent", "ActionRequest", "hks_", "HKS"]
sc = idautils.Strings()
sc.setup(strtypes=[ida_nalt.STRTYPE_C, ida_nalt.STRTYPE_C_16])
nstr = 0
for s in sc:
    try:
        t = str(s)
    except Exception:  # noqa: BLE001
        continue
    if any(k in t for k in keys) and len(t) < 120:
        refs = []
        for x in idautils.XrefsTo(s.ea):
            g = ida_funcs.get_func(x.frm)
            refs.append(f"{x.frm:#x}/{(g.start_ea if g else 0):#x}")
            if len(refs) >= 6:
                break
        summary.append(f"STR {s.ea:#x} {t!r} refs {refs}")
        nstr += 1
        if nstr > 400:
            break

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
idc.qexit(0)
