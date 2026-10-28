# RE-FINDINGS-2: boss gauge, anim id, other flag funcs, warp/rest helpers
import os, ida_funcs, ida_hexrays, ida_name, ida_pro, idautils, idc, ida_bytes, ida_ua
OUT_DIR = r"F:\SekiroRE\outRE2\boss"
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

def dec(ea, label, depth=0, maxdepth=0, fptrs=False):
    f = ida_funcs.get_func(ea)
    if f is None or f.start_ea in done: return
    done.add(f.start_ea)
    try: text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e: text = f"// failed: {e}"
    nx = len(list(idautils.CodeRefsTo(f.start_ea, 0)))
    open(os.path.join(OUT_DIR, f"{label}_{f.start_ea-BASE:x}.c"), "w", encoding="utf-8").write(
        f"// {label} rva {f.start_ea-BASE:#x} xrefs {nx}\n" + "\n".join("//" + l for l in prologue(f.start_ea).split("\n")) + "\n" + text)
    summary.append(f"{'  '*depth}{label} rva {f.start_ea-BASE:#x} size {f.end_ea-f.start_ea:#x} xrefs {nx}")
    e2 = f.start_ea
    while e2 < f.end_ea:
        m = idc.print_insn_mnem(e2)
        if depth < maxdepth and m in ("call", "jmp"):
            t = idc.get_operand_value(e2, 0)
            if TEXT_LO <= t < TEXT_HI and t != f.start_ea: dec(t, label + "_c", depth + 1, maxdepth, fptrs)
        if fptrs and m == "lea":
            t = idc.get_operand_value(e2, 1)
            if TEXT_LO <= t < TEXT_HI and ida_funcs.get_func(t) and ida_funcs.get_func(t).start_ea == t:
                summary.append(f"{'  '*depth}  fptr {t-BASE:#x} @ {e2-BASE:#x}")
                dec(t, label + "_fp", depth + 1, maxdepth, False)
        e2 = idc.next_head(e2, f.end_ea)

# Lua/ESD-binding registration funcs for strings found in re2_misc
for rva, lab in [(0x93490, "SetBossGauge"), (0x93500, "ClearBossGauge"), (0x922a0, "BossLockCam"),
                 (0x81e90, "IsExistAnimID"), (0x9a080, "ReportBossArea")]:
    dec(BASE + rva, lab, 0, 0, True)
# Other flag functions near Set/Get
for rva in (0x6c3ed0, 0x6c47a0, 0x6c4890, 0x6c48c0, 0x6c4920, 0x6c4980, 0x6c55d0, 0x6aaab0, 0x6aa860, 0x1130de0):
    dec(BASE + rva, "flagfn")
# Warp / rest helpers
for rva in (0x6d7070, 0x832bb0, 0x832bc0, 0x832790, 0x832100, 0x832c90, 0x832be0, 0x832cb0, 0x76e7c0, 0x76deb0):
    dec(BASE + rva, "warpfn")
# ForceAnimation + TimeAct module methods
dec(BASE + 0x6D5A90, "ForceAnim", 0, 1)
dec(BASE + 0xBD84C0, "ForceAnimByEvMod", 0, 1)
for fea in idautils.Functions(BASE + 0xB5B000, BASE + 0xB5D800):
    dec(fea, "ta")
# string refs for 'animId' style debug prints in TimeAct region
for s in idautils.Strings():
    t = str(s)
    if ("%d" in t or "%x" in t) and ("Anim" in t or "anim" in t or "TAE" in t or "Tae" in t) and len(t) < 120:
        refs = [ida_funcs.get_func(x).start_ea - BASE for x in idautils.DataRefsTo(s.ea) if ida_funcs.get_func(x)]
        summary.append(f"STR {s.ea-BASE:#x} {t!r} refs {[hex(r) for r in refs[:6]]}")
open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8").write("\n".join(summary))
ida_pro.qexit(0)
