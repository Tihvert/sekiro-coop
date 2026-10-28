# RE-FINDINGS-2: idol request globals (0x143AFEB9x), flag-sync helper, hook prologues
import os, ida_funcs, ida_hexrays, ida_name, ida_pro, idautils, idc, ida_bytes, ida_ua
OUT_DIR = r"F:\SekiroRE\outRE2\idol"
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
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

def dec(ea, label):
    f = ida_funcs.get_func(ea)
    if f is None or f.start_ea in done: return
    done.add(f.start_ea)
    try: text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e: text = f"// failed: {e}"
    nx = len(list(idautils.CodeRefsTo(f.start_ea, 0)))
    open(os.path.join(OUT_DIR, f"{label}_{f.start_ea-BASE:x}.c"), "w", encoding="utf-8").write(f"// {label} rva {f.start_ea-BASE:#x} xrefs {nx}\n" + text)
    summary.append(f"{label} rva {f.start_ea-BASE:#x} size {f.end_ea-f.start_ea:#x} xrefs {nx}")

for g in range(0x143AFEB90, 0x143AFEBB0, 4):
    refs = list(idautils.DataRefsTo(g))
    summary.append(f"== global {g:#x} refs {len(refs)}")
    for x in refs[:12]:
        f = ida_funcs.get_func(x)
        kind = "W" if (idc.print_insn_mnem(x) == "mov" and idc.get_operand_type(x, 0) == idc.o_mem) else "R"
        summary.append(f"   {kind} {x-BASE:#x} in {(f.start_ea-BASE) if f else 0:#x}: {idc.generate_disasm_line(x,0)}")
        if f and kind == "W": dec(f.start_ea, f"w{g&0xff:02x}")
for rva in (0x76E7B0, 0x76E7D0, 0x76EE60, 0x76DEB0, 0x76DF90, 0x8D9ED0, 0x1409EEEC0 - BASE, 0x14076E080 - BASE):
    dec(BASE + rva, "idolfn")
for rva, lab in [(0x6C4520, "SetEvent"), (0x6C3E60, "GetEvent"), (0x6C47A0, "SetEventValue"), (0x6AAAB0, "SetEventWrap"),
                 (0xA2BE40, "Rest"), (0x68E870, "Warp"), (0x67C7C0, "SetBossGaugeImpl"), (0x8C3A20, "ClearBossGauge"),
                 (0x83B150, "GameRendCamUpdate"), (0xB5C910, "TA_SetF4")]:
    summary.append(f"== prologue {lab} {rva:#x}\n" + prologue(BASE + rva))
open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8").write("\n".join(summary))
ida_pro.qexit(0)
