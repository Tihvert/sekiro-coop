# RE-FINDINGS-2: event flag set/get + rest + RTTI class list for camera/boss
import os, ida_funcs, ida_hexrays, ida_name, ida_pro, idautils, idc, ida_bytes, ida_ua
OUT_DIR = r"F:\SekiroRE\outRE2\flags"
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
TEXT_LO, TEXT_HI = 0x140001000, 0x14291D000
BASE = 0x140000000
summary = []; done = set()

def prologue(ea, n=24):
    out = []; cur = ea
    while cur < ea + n:
        insn = ida_ua.insn_t(); l = ida_ua.decode_insn(insn, cur)
        if l <= 0: break
        b = ida_bytes.get_bytes(cur, l).hex(" ")
        out.append(f"    +{cur-ea:02x}: {b:<30} {idc.generate_disasm_line(cur, 0)}")
        cur += l
    return "\n".join(out)

def dec(ea, label, depth=0, maxdepth=1):
    f = ida_funcs.get_func(ea)
    if f is None or f.start_ea in done: return
    done.add(f.start_ea)
    try: text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e: text = f"// failed: {e}"
    nx = len(list(idautils.CodeRefsTo(f.start_ea, 0)))
    open(os.path.join(OUT_DIR, f"{label}_{f.start_ea-BASE:x}.c"), "w", encoding="utf-8").write(
        f"// {label} rva {f.start_ea-BASE:#x} xrefs {nx}\n// prologue:\n" + "\n".join("//" + l for l in prologue(f.start_ea).split("\n")) + "\n" + text)
    summary.append(f"{'  '*depth}{label} rva {f.start_ea-BASE:#x} size {f.end_ea-f.start_ea:#x} xrefs {nx}")
    if depth < maxdepth:
        e2 = f.start_ea
        while e2 < f.end_ea:
            if idc.print_insn_mnem(e2) in ("call", "jmp"):
                t = idc.get_operand_value(e2, 0)
                if TEXT_LO <= t < TEXT_HI and t != f.start_ea: dec(t, label + "_c", depth + 1, maxdepth)
            e2 = idc.next_head(e2, f.end_ea)

for rva, lab in [(0x6C4520, "SetEvent"), (0x6C3E60, "GetEvent"), (0xA2BE40, "Rest"), (0x68E870, "Warp")]:
    summary.append(f"== {lab} prologue\n" + prologue(BASE + rva))
    dec(BASE + rva, lab, 0, 2)

# EventFlagMan global xrefs
g = BASE + 0x3D55FE8
summary.append(f"== xrefs to EventFlagMan global {g:#x} name {ida_name.get_name(g)}")
fs = set()
for x in idautils.DataRefsTo(g):
    f = ida_funcs.get_func(x)
    if f: fs.add(f.start_ea)
summary.append(f"  {len(fs)} functions")
for s in sorted(fs)[:400]:
    summary.append(f"  {s-BASE:#x} {ida_name.get_name(s)} xrefsTo {len(list(idautils.CodeRefsTo(s,0)))}")

# RTTI / names of interest
keys = ("Cam", "EventFlag", "Boss", "Idol", "Bonfire", "Warp", "Rest", "GameRend", "TimeAct", "FieldArea", "Viewport", "Frustum")
for ea, name in idautils.Names():
    dn = ida_name.demangle_name(name, 0) or name
    if "vftable" in dn and any(k in dn for k in keys):
        summary.append(f"VT {ea-BASE:#x} {dn}")
open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8").write("\n".join(summary))
ida_pro.qexit(0)
