# brute-force: find E8/E9 rel32 and lea rip-rel referencing targets -> F:\SekiroRE\out34\scan.txt
import os, ida_bytes, ida_segment, idautils, idc, ida_funcs
OUT = r"F:\SekiroRE\out34"; os.makedirs(OUT, exist_ok=True)
B = 0x140000000
targets = {B + 0xB42390: "notify", B + 0xB44590: "bm_onevent", B + 0xFED450: "rrec"}
res = []
for seg_ea in idautils.Segments():
    s = ida_segment.getseg(seg_ea)
    if not (s.perm & ida_segment.SEGPERM_EXEC):
        continue
    data = ida_bytes.get_bytes(s.start_ea, s.end_ea - s.start_ea)
    n = len(data)
    import struct
    for i in range(n - 7):
        # rel32 at i+1 (call/jmp) or at i+3 (lea/mov rip-rel with 3-byte opcode)
        for off, ln in ((1, 5), (2, 6), (3, 7)):
            if i + off + 4 > n:
                continue
            d = struct.unpack_from("<i", data, i + off)[0]
            t = s.start_ea + i + ln + d
            if t in targets:
                ea = s.start_ea + i
                f = ida_funcs.get_func(ea)
                res.append(f"{targets[t]} <- {ea:#x} len{ln} bytes {data[i:i+ln].hex()} fn {(f.start_ea if f else 0):#x} {idc.generate_disasm_line(ea,0)}")
open(os.path.join(OUT, "scan.txt"), "w").write("\n".join(res))
idc.qexit(0)
