# IDAPython (IDA 9.3, headless) on the analysed quick.i64: decompile the
# CSDebugChrCreator step-method table. Entries are {fn, wchar_t* name}
# (16 bytes; see vft09: `*(a1+8) + 16*state`), so stride 16.
import os
import ida_bytes
import ida_funcs
import ida_hexrays
import ida_pro
import idc

OUT_DIR = r"F:\SekiroRE\out2"
STEPS = 0x143D88050
TEXT_LO, TEXT_HI = 0x140001000, 0x14291D000
os.makedirs(OUT_DIR, exist_ok=True)
summary = []
done = set()


def wstr(ea):
    s = []
    for i in range(200):
        c = ida_bytes.get_word(ea + 2 * i)
        if c == 0:
            break
        s.append(chr(c))
    return "".join(s)


def decompile(ea, label, depth=0):
    f = ida_funcs.get_func(ea)
    if f is None or f.start_ea in done:
        return
    done.add(f.start_ea)
    try:
        text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e:  # noqa: BLE001
        text = f"// decompile failed: {e}\n"
    with open(os.path.join(OUT_DIR, f"{label}_{f.start_ea:x}.c"), "w", encoding="utf-8") as fh:
        fh.write(f"// {label} at {f.start_ea:#x}\n{text}")
    summary.append(f"{label} {f.start_ea:#x} ({len(text)} chars)")
    # Follow direct callees one or two levels: the actual spawn call lives there.
    if depth < 2:
        for item in idc.get_func_attr(f.start_ea, idc.FUNCATTR_START), :
            pass
        ea2 = f.start_ea
        while ea2 < f.end_ea:
            if idc.print_insn_mnem(ea2) == "call":
                tgt = idc.get_operand_value(ea2, 0)
                if TEXT_LO <= tgt < TEXT_HI:
                    decompile(tgt, f"{label}_c", depth + 1)
            ea2 = idc.next_head(ea2, f.end_ea)


ida_hexrays.init_hexrays_plugin()
for i in range(8):
    fn = ida_bytes.get_qword(STEPS + 16 * i)
    name_ptr = ida_bytes.get_qword(STEPS + 16 * i + 8)
    if not (TEXT_LO <= fn < TEXT_HI):
        summary.append(f"steps end at {i}")
        break
    summary.append(f"step{i} {fn:#x} name={wstr(name_ptr)!r}")
    decompile(fn, f"step{i}")

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
