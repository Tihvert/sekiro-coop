# RE-FINDINGS-2: rest/warp callers, flag resolver callers, camera classes
import os, ida_funcs, ida_hexrays, ida_name, ida_pro, idautils, idc, ida_bytes, ida_ua
OUT_DIR = r"F:\SekiroRE\outRE2\cam"
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
        out.append(f"    +{cur-ea:02x}: {ida_bytes.get_bytes(cur, l).hex(' '):<30} {idc.generate_disasm_line(cur, 0)}")
        cur += l
    return "\n".join(out)

def dec(ea, label, depth=0, maxdepth=0):
    f = ida_funcs.get_func(ea)
    if f is None or f.start_ea in done: return
    done.add(f.start_ea)
    try: text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e: text = f"// failed: {e}"
    nx = len(list(idautils.CodeRefsTo(f.start_ea, 0)))
    open(os.path.join(OUT_DIR, f"{label}_{f.start_ea-BASE:x}.c"), "w", encoding="utf-8").write(
        f"// {label} rva {f.start_ea-BASE:#x} xrefs {nx}\n" + "\n".join("//" + l for l in prologue(f.start_ea).split("\n")) + "\n" + text)
    summary.append(f"{'  '*depth}{label} rva {f.start_ea-BASE:#x} size {f.end_ea-f.start_ea:#x} xrefs {nx}")
    if depth < maxdepth:
        e2 = f.start_ea
        while e2 < f.end_ea:
            if idc.print_insn_mnem(e2) in ("call", "jmp"):
                t = idc.get_operand_value(e2, 0)
                if TEXT_LO <= t < TEXT_HI and t != f.start_ea: dec(t, label + "_c", depth + 1, maxdepth)
            e2 = idc.next_head(e2, f.end_ea)

def callers(ea, label, maxn=12):
    fs = []
    for x in idautils.CodeRefsTo(ea, 0):
        f = ida_funcs.get_func(x)
        if f and f.start_ea not in fs: fs.append(f.start_ea)
    summary.append(f"== callers of {label} {ea-BASE:#x}: " + ", ".join(f"{s-BASE:#x}" for s in fs))
    for s in fs[:maxn]: dec(s, label + "_caller")

dec(BASE + 0x68E870, "Warp", 0, 1)
callers(BASE + 0x68E870, "Warp")
callers(BASE + 0xA2BE40, "Rest")
dec(BASE + 0xA344A0, "Rest_sub344a0"); dec(BASE + 0x9E99A0, "Rest_sub9e99a0")
callers(BASE + 0x6C63F0, "FlagResolve")
callers(BASE + 0x6C4520, "SetEvent", 40)

def vt(name_sub, label, n=40, decn=0):
    for ea, name in idautils.Names():
        dn = ida_name.demangle_name(name, 0) or name
        if dn == f"const {name_sub}::`vftable'":
            summary.append(f"== VT {label} {ea-BASE:#x}")
            for i in range(n):
                v = idc.get_qword(ea + 8*i)
                if not (TEXT_LO <= v < TEXT_HI): break
                summary.append(f"  [{i}] +{8*i:#x} {v-BASE:#x}")
                if i < decn: dec(v, f"{label}_v{i}")
            # ctor-ish users of the vtable
            for x in idautils.DataRefsTo(ea):
                f = ida_funcs.get_func(x)
                if f:
                    summary.append(f"  vt ref in {f.start_ea-BASE:#x}")
                    dec(f.start_ea, f"{label}_vtref")
            return
vt("NS_SPRJ::SprjCam", "SprjCam", 30, 12)
vt("NS_SPRJ::SprjPersCam", "SprjPersCam", 30, 12)
vt("NS_SPRJ::GameRend", "GameRend", 30, 0)
vt("NS_SPRJ::SprjCameraImp", "SprjCameraImp", 30, 0)
vt("NS_SPRJ::CameraMan", "CameraMan", 30, 0)
vt("NS_SPRJ::ChrCam", "ChrCam", 30, 0)
vt("NS_SPRJ::SprjEventFlagMan", "SprjEventFlagMan", 40, 0)
open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8").write("\n".join(summary))
ida_pro.qexit(0)
