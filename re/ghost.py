# IDAPython: find how the game creates ReplayGhostIns (1.06 "Remnant" ghosts,
# player model + replayed animation). Locate vftables by IDA's RTTI names,
# decompile every function referencing them (ctors) and their callers.
import os
import ida_funcs
import ida_hexrays
import ida_name
import ida_pro
import ida_xref
import idautils
import idc

OUT_DIR = r"F:\SekiroRE\out5"
WANT = ["ReplayGhostIns", "CSDisplayGhost", "ReplayManipulator", "WanderingGhost", "PlayerIns::`vftable"]
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
summary, done = [], set()


def dec(ea, label):
    f = ida_funcs.get_func(ea)
    if f is None or f.start_ea in done:
        return f
    done.add(f.start_ea)
    try:
        text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e:  # noqa: BLE001
        text = f"// failed: {e}"
    with open(os.path.join(OUT_DIR, f"{label}_{f.start_ea:x}.c"), "w", encoding="utf-8") as fh:
        fh.write(f"// {label} {f.start_ea:#x}\n{text}")
    summary.append(f"{label} {f.start_ea:#x} len {len(text)}")
    return f


def callers(ea, label, depth):
    if depth == 0:
        return
    n = 0
    ref = ida_xref.get_first_cref_to(ea)
    while ref != idc.BADADDR and n < 5:
        f = dec(ref, label)
        if f:
            callers(f.start_ea, label + "u", depth - 1)
        n += 1
        ref = ida_xref.get_next_cref_to(ea, ref)


for ea, name in idautils.Names():
    dn = ida_name.demangle_name(name, 0) or name
    if "vftable" not in dn or not any(w in dn for w in WANT):
        continue
    tag = "".join(ch for ch in dn.split("::")[-2] if ch.isalnum())[:24] if "::" in dn else "vt"
    summary.append(f"VFTABLE {ea:#x} {dn}")
    ref = ida_xref.get_first_dref_to(ea)
    while ref != idc.BADADDR:
        f = dec(ref, f"{tag}_ref")
        if f and "ReplayGhostIns" in dn:
            callers(f.start_ea, f"{tag}_up", 3)
        ref = ida_xref.get_next_dref_to(ea, ref)

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
