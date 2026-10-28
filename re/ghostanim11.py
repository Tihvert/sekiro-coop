# IDAPython (headless): ghost animation part 2, pass 9 -> F:\SekiroRE\out39
# Locomotion variable table near 0x143B08E40 (MoveAngle / MoveSpeedLevel / MoveDirection): contents and users.
import os, ida_bytes, ida_funcs, ida_hexrays, ida_name, ida_segment, idautils, idc

OUT_DIR = r"F:\SekiroRE\out39"
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
summary, done = [], set()
B = 0x140000000


def dec(ea, label):
    f = ida_funcs.get_func(ea)
    if f is None or f.start_ea in done:
        return
    done.add(f.start_ea)
    try:
        text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e:  # noqa: BLE001
        text = f"// failed: {e}"
    with open(os.path.join(OUT_DIR, f"{label}_{f.start_ea:x}.c"), "w", encoding="utf-8") as fh:
        fh.write(f"// {label} {f.start_ea:#x}\n{text}")
    summary.append(f"{label} {f.start_ea:#x} len {len(text)}")


def wstr(ea):
    out = []
    for i in range(64):
        c = ida_bytes.get_word(ea + 2 * i)
        if c == 0:
            break
        out.append(chr(c) if 32 <= c < 127 else "?")
    return "".join(out)


lo, hi = B + 0x3B08C00, B + 0x3B09400
ea = lo
while ea < hi:
    q = ida_bytes.get_qword(ea)
    d = ida_bytes.get_dword(ea)
    refs = [f"{x.frm:#x}" for x in idautils.XrefsTo(ea)][:4]
    s = wstr(q) if B + 0x2900000 <= q < B + 0x2D00000 else ""
    summary.append(f"{ea:#x}: {q:#018x} {s!r} refs {refs}")
    for x in idautils.XrefsTo(ea):
        f = ida_funcs.get_func(x.frm)
        if f:
            dec(f.start_ea, "tbluser")
    ea += 8

for e, l in [(0x13779F0, "gen_check"), (0xFF3100, "get_b"), (0xFF3250, "get_f"), (0xFF32D0, "set_b"), (0xFF33B0, "set_s16"),
             (0xFF3490, "set_i32"), (0xFF3720, "var_name"), (0x9F0D70, "x9f0d70")]:
    dec(B + e, l)

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
idc.qexit(0)
