# IDAPython (headless): ghost animation part 2, pass 3 -> F:\SekiroRE\out32
# Sync recorder R (behaviorModule+0x60): init (synced variable lists), per-frame update, event record path.
import os, ida_bytes, ida_funcs, ida_hexrays, ida_name, ida_segment, idautils, idc

OUT_DIR = r"F:\SekiroRE\out32"
os.makedirs(OUT_DIR, exist_ok=True)
ida_hexrays.init_hexrays_plugin()
summary, done = [], set()
B = 0x140000000


def is_code(ea):
    s = ida_segment.getseg(ea)
    return s is not None and (s.perm & ida_segment.SEGPERM_EXEC) != 0


def dec(ea, label, depth=0, maxdepth=0):
    f = ida_funcs.get_func(ea)
    if f is None:
        summary.append(f"{'  ' * depth}{label} {ea:#x} NOFUNC")
        return
    if f.start_ea in done:
        return
    done.add(f.start_ea)
    try:
        text = str(ida_hexrays.decompile(f.start_ea))
    except Exception as e:  # noqa: BLE001
        text = f"// failed: {e}"
    with open(os.path.join(OUT_DIR, f"{label}_{f.start_ea:x}.c"), "w", encoding="utf-8") as fh:
        fh.write(f"// {label} {f.start_ea:#x}\n{text}")
    summary.append(f"{'  ' * depth}{label} {f.start_ea:#x} len {len(text)}")
    if depth < maxdepth:
        ea2 = f.start_ea
        while ea2 < f.end_ea:
            if idc.print_insn_mnem(ea2) in ("call", "jmp"):
                t = idc.get_operand_value(ea2, 0)
                if is_code(t) and not (f.start_ea <= t < f.end_ea):
                    dec(t, label + "_c", depth + 1, maxdepth)
            ea2 = idc.next_head(ea2, f.end_ea)


def callers(ea, label, n=12, maxdepth=0):
    out = []
    for x in idautils.XrefsTo(ea):
        g = ida_funcs.get_func(x.frm)
        out.append((x.frm, g.start_ea if g else None))
    summary.append(f"CALLERS of {ea:#x} ({len(out)}): {[(hex(a), hex(b) if b else None) for a, b in out[:60]]}")
    for a, b in out[:n]:
        if b:
            dec(b, label, 0, maxdepth)
    return out


def prologue(ea, n=8):
    summary.append(f"PROLOGUE {ea:#x}:")
    h = ea
    for _ in range(n):
        size = idc.get_item_size(h)
        b = " ".join(f"{ida_bytes.get_byte(h + i):02X}" for i in range(size))
        summary.append(f"  +{h - ea:02X} {b:<32} {idc.generate_disasm_line(h, 0)}")
        h = idc.next_head(h, ea + 0x100)


for e, l, d in [(0xFEFB50, "vlist1", 1), (0xFEF600, "vlist2", 1), (0xFEF030, "vlist3", 1), (0xFEE640, "rreset", 1),
                (0xFED670, "rupd", 1), (0xFEEB20, "evadd", 1), (0xFEEBC0, "evadd2", 1), (0xFF0530, "vecpush", 0),
                (0xB44590, "b44590", 1), (0xB45440, "b45440", 0), (0xB438A0, "b438a0", 0), (0xB46E70, "b46e70", 0),
                (0xB3FCE0, "b3fce0", 0), (0xB3FCF0, "b3fcf0", 0), (0xFF06B0, "setvar_name", 0), (0xFF0610, "setvar_init", 0),
                (0x8D7000, "frm_encode", 1), (0x8DCF90, "frm_decode", 1), (0x8DC780, "frm_reset", 1),
                (0x8D6020, "frm_d6020", 0), (0xB4F790, "b4f790", 0), (0x7EEB50, "capcaller", 0), (0x7E54F0, "e54f0", 0),
                (0xB4F0F0, "b4f0f0", 0), (0xB4EF70, "b4ef70", 0), (0xB4EF10, "b4ef10", 0), (0x8D6FF0, "frm_d6ff0", 0),
                (0xA3A170, "a3a170", 0)]:
    dec(B + e, l, 0, d)

for e, l in [(0xFED450, "evrec_caller"), (0xB44590, "b44590_caller"), (0xFED670, "rupd_caller"), (0xB45440, "b45440_caller"),
             (0xFED630, "rinit_caller"), (0xFEEB20, "evadd_caller"), (0xFEEBC0, "evadd2_caller"),
             (0x8D7000, "enc_caller"), (0x8DCF90, "decd_caller")]:
    callers(B + e, l, 10, 0)

for e in (0xFED450, 0xB44590, 0xFED670, 0xB4F1A0, 0xB4EFE0, 0xFEC790, 0xFECE80, 0xFED3D0):
    prologue(B + e)

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
idc.qexit(0)
