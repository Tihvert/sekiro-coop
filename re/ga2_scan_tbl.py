# brute-force: rip-relative refs into the variable-name table 0x143B08E28..0x143B09400 -> F:\SekiroRE\out40\scan.txt
import os, struct, ida_bytes, ida_segment, idautils, idc, ida_funcs, ida_hexrays
OUT = r"F:\SekiroRE\out40"; os.makedirs(OUT, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
B = 0x140000000
LO, HI = B + 0x3B08E28, B + 0x3B09800
res, fns = [], set()
for seg_ea in idautils.Segments():
    s = ida_segment.getseg(seg_ea)
    if not (s.perm & ida_segment.SEGPERM_EXEC):
        continue
    data = ida_bytes.get_bytes(s.start_ea, s.end_ea - s.start_ea)
    n = len(data)
    for i in range(n - 7):
        if data[i] in (0x48, 0x4C) and data[i + 1] in (0x8D, 0x8B) and (data[i + 2] & 0xC7) == 0x05:
            d = struct.unpack_from("<i", data, i + 3)[0]
            t = s.start_ea + i + 7 + d
            if LO <= t < HI:
                ea = s.start_ea + i
                f = ida_funcs.get_func(ea)
                res.append(f"{t:#x} <- {ea:#x} fn {(f.start_ea if f else 0):#x} {idc.generate_disasm_line(ea, 0)}")
                if f:
                    fns.add(f.start_ea)
for fe in sorted(fns):
    try:
        t = str(ida_hexrays.decompile(fe))
    except Exception as e:
        t = f"// failed {e}"
    open(os.path.join(OUT, f"tbl_{fe:x}.c"), "w", encoding="utf-8").write(t)
open(os.path.join(OUT, "scan.txt"), "w").write("\n".join(res))
idc.qexit(0)
