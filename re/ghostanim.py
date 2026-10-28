# IDAPython (headless): how to play an animation on a ReplayGhostIns.
# Output: F:\SekiroRE\out16 (one .c per function + summary.txt). Static only.
import os, ida_bytes, ida_funcs, ida_hexrays, ida_name, ida_pro, ida_segment, idautils, idc

OUT_DIR = r"F:\SekiroRE\out16"
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
        summary.append(f"{'  ' * depth}{label} {f.start_ea:#x} (dup)")
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
                if is_code(t) and t != f.start_ea and not (f.start_ea <= t < f.end_ea):
                    dec(t, label + "_c", depth + 1, maxdepth)
            ea2 = idc.next_head(ea2, f.end_ea)


def vt_dump(vt, tag, slots, maxdepth=0):
    for i in slots:
        v = ida_bytes.get_qword(vt + 8 * i)
        summary.append(f"{tag}[{i}] (+{8*i:#x}) = {v:#x}")
        if is_code(v):
            dec(v, f"{tag}{i:03d}", 0, maxdepth)


# 1. Module vtables by RTTI name.
mod_vts = {}
for ea, name in idautils.Names():
    dn = ida_name.demangle_name(name, 0) or name
    if "vftable" in dn and ("SprjChr" in dn or "ChrIns" in dn or "Replay" in dn) and "Module" in dn:
        mod_vts[dn] = ea
for dn, ea in sorted(mod_vts.items()):
    xr = [x.frm for x in idautils.XrefsTo(ea)]
    summary.append(f"VT {ea:#x} {dn} xrefs {[hex(x) for x in xr[:6]]}")

# 2. vtbl[247] of PlayerIns / ReplayGhostIns (what the EMEVD path calls).
PLAYER_VT, GHOST_VT = 0x142A2B338, 0x142A2D190
for vt, n in [(PLAYER_VT, "player"), (GHOST_VT, "ghost")]:
    vt_dump(vt, f"{n}_vt", [94, 245, 246, 247, 248, 249], 1)

# 3. Event module vtable, all slots up to 16.
for dn, ea in mod_vts.items():
    if "SprjChrEventModule::`vftable" in dn:
        vt_dump(ea, "evmod", range(0, 16), 2)
    if "SprjChrBehaviorSyncModule::`vftable" in dn:
        vt_dump(ea, "bsync", range(0, 12), 1)
    if "SprjChrTimeActModule::`vftable" in dn:
        vt_dump(ea, "tamod", range(0, 12), 0)

# 4. Specific functions.
for ea, l in [
    (0x140BD84C0, "force_by_evmod"),
    (0x140B271D0, "bsync_owner"),
    (0x140FFEBB0, "bsync_apply_528"),
    (0x140B4E9C0, "bsync_apply_496"),
    (0x140FEC040, "bsync_apply_9728"),
    (0x1409A8150, "data_frame"),
]:
    dec(ea, l, 0, 1)

# 5. Who constructs the event module? (xrefs to its vftable -> ctor -> callers)
for dn, ea in mod_vts.items():
    if "SprjChrEventModule::`vftable" in dn or "SprjChrBehaviorModule::`vftable" in dn:
        for x in idautils.XrefsTo(ea):
            f = ida_funcs.get_func(x.frm)
            if f is None:
                continue
            dec(f.start_ea, "ctor_" + ("ev" if "Event" in dn else "beh"), 0, 0)
            for y in idautils.XrefsTo(f.start_ea):
                g = ida_funcs.get_func(y.frm)
                if g is not None:
                    summary.append(f"  ctor {f.start_ea:#x} called from {y.frm:#x} in {g.start_ea:#x}")
                    dec(g.start_ea, "ctorcaller", 0, 0)

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
