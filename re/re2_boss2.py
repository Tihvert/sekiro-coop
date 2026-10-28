# RE-FINDINGS-2: boss gauge backend, TimeAct format string, ChrCam ctor, flag helper users
import os, ida_funcs, ida_hexrays, ida_name, ida_pro, idautils, idc, ida_bytes, ida_ua
OUT_DIR = r"F:\SekiroRE\outRE2\boss2"
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
TEXT_LO, TEXT_HI = 0x140001000, 0x14291D000
BASE = 0x140000000
summary = []; done = set()

def dec(ea, label, depth=0, maxdepth=0):
    f = ida_funcs.get_func(ea)
    if f is None or f.start_ea in done: return
    done.add(f.start_ea)
    try: text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e: text = f"// failed: {e}"
    nx = len(list(idautils.CodeRefsTo(f.start_ea, 0)))
    open(os.path.join(OUT_DIR, f"{label}_{f.start_ea-BASE:x}.c"), "w", encoding="utf-8").write(f"// {label} rva {f.start_ea-BASE:#x} xrefs {nx}\n" + text)
    summary.append(f"{'  '*depth}{label} rva {f.start_ea-BASE:#x} size {f.end_ea-f.start_ea:#x} xrefs {nx}")
    if depth < maxdepth:
        e2 = f.start_ea
        while e2 < f.end_ea:
            if idc.print_insn_mnem(e2) in ("call", "jmp"):
                t = idc.get_operand_value(e2, 0)
                if TEXT_LO <= t < TEXT_HI and t != f.start_ea: dec(t, label + "_c", depth + 1, maxdepth)
            e2 = idc.next_head(e2, f.end_ea)

p = idc.get_qword(0x142A7B6C0)
s = idc.get_strlit_contents(p, -1, idc.STRTYPE_C_16) if p else None
summary.append(f"fmt qword_142A7B6C0 -> {p:#x} {s!r}")
dec(BASE + 0x67C7C0, "SetBossGaugeImpl", 0, 1)
dec(BASE + 0x8C3A20, "ClearBossGaugeImpl", 0, 1)
g = 0x143D55600
summary.append(f"global {g:#x} name {ida_name.get_name(g)}")
fs = sorted({ida_funcs.get_func(x).start_ea for x in idautils.DataRefsTo(g) if ida_funcs.get_func(x)})
summary.append(f"  refs {len(fs)}: " + ", ".join(f"{x-BASE:#x}" for x in fs[:60]))
# RTTI of object stored in that global: find ctor writing it
for x in idautils.DataRefsTo(g):
    if idc.print_insn_mnem(x) == "mov" and idc.get_operand_type(x, 0) == idc.o_mem:
        summary.append(f"  store at {x-BASE:#x}"); f = ida_funcs.get_func(x)
        if f: dec(f.start_ea, "g55600_store")
dec(BASE + 0x733850, "ChrCamCtor")
dec(BASE + 0x734240, "CamGetMatrix")
dec(BASE + 0x6C3ED0, "flag3ed0")
dec(BASE + 0x6C47A0, "flag47a0")
open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8").write("\n".join(summary))
ida_pro.qexit(0)
