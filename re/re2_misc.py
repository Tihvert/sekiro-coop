# RE-FINDINGS-2: GameRend methods, resolution globals, boss/anim strings, TimeAct module vtable
import os, re, ida_funcs, ida_hexrays, ida_name, ida_pro, idautils, idc, ida_bytes, ida_ua
OUT_DIR = r"F:\SekiroRE\outRE2\misc"
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
TEXT_LO, TEXT_HI = 0x140001000, 0x14291D000
BASE = 0x140000000
summary = []; done = set()

def dec(ea, label):
    f = ida_funcs.get_func(ea)
    if f is None or f.start_ea in done: return None
    done.add(f.start_ea)
    try: text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e: text = f"// failed: {e}"
    nx = len(list(idautils.CodeRefsTo(f.start_ea, 0)))
    open(os.path.join(OUT_DIR, f"{label}_{f.start_ea-BASE:x}.c"), "w", encoding="utf-8").write(
        f"// {label} rva {f.start_ea-BASE:#x} xrefs {nx}\n" + text)
    summary.append(f"{label} rva {f.start_ea-BASE:#x} size {f.end_ea-f.start_ea:#x} xrefs {nx}")
    return text

# 1. GameRend method range
for fea in idautils.Functions(BASE + 0x839000, BASE + 0x83D000):
    dec(fea, "gr")
# 2. resolution globals
for g in (0x143B1D100, 0x143B1D104):
    fs = sorted({ida_funcs.get_func(x).start_ea for x in idautils.DataRefsTo(g) if ida_funcs.get_func(x)})
    summary.append(f"== refs to {g:#x}: {len(fs)} funcs " + ", ".join(f"{s-BASE:#x}" for s in fs[:40]))
    for s in fs[:6]: dec(s, f"res{g&0xf:x}")
# 3. strings
pat = re.compile(r"boss|Boss|BOSS|AnimId|animId|AnimID|TimeAct|CurrentAnim|Fov|FOV|fovy|ViewMatrix|ProjMatrix|BossGauge|EnemyGauge|bossHp|BossHp", re.I if False else 0)
for s in idautils.Strings():
    t = str(s)
    if pat.search(t) and len(t) < 160:
        refs = [ida_funcs.get_func(x).start_ea - BASE for x in idautils.DataRefsTo(s.ea) if ida_funcs.get_func(x)]
        summary.append(f"STR {s.ea-BASE:#x} {t!r} refs {[hex(r) for r in refs[:6]]}")
# 4. TimeAct module vtable
for ea, name in idautils.Names():
    dn = ida_name.demangle_name(name, 0) or name
    if dn in ("const NS_SPRJ::SprjChrTimeActModule::`vftable'",):
        summary.append(f"== VT {dn} {ea-BASE:#x}")
        for i in range(24):
            v = idc.get_qword(ea + 8*i)
            if not (TEXT_LO <= v < TEXT_HI): break
            summary.append(f"  [{i}] {v-BASE:#x}")
            dec(v, f"ta_v{i}")
        for x in idautils.DataRefsTo(ea):
            f = ida_funcs.get_func(x)
            if f: dec(f.start_ea, "ta_vtref")
    if "FD4DebugMenuCBMemberFuncHolder<class NS_SPRJ::SprjChrTimeActModule" in dn or "FD4DebugMenuCBMemberFuncHolder<class NS_SPRJ::ChrCam," in dn:
        for x in idautils.DataRefsTo(ea):
            f = ida_funcs.get_func(x)
            if f: dec(f.start_ea, "dbgholder")
open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8").write("\n".join(summary))
ida_pro.qexit(0)
