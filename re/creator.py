# IDAPython (IDA 9.3, headless): decompile CSDebugChrCreator code from the
# decrypted sekiro.exe dump (base 0x140000000).
#
# Targets (found live by the mod's F11 RTTI/creator dump, 2026-10-22):
#   vftable          0x142A952A8  (creator +0x00)
#   stepper table?   0x143D88050  (creator +0x08, ER calls this stepper_fns)
# Plus every function that references the vftable (ctor/dtor).
#
# Output: one .c file per function in OUT_DIR, and summary.txt.
# Runs without full auto-analysis: functions are created on demand.

import os
import ida_auto
import ida_bytes
import ida_funcs
import ida_hexrays
import ida_name
import ida_pro
import ida_xref
import idc

OUT_DIR = r"F:\SekiroRE\out"
VFTABLE = 0x142A952A8
STEPPERS = 0x143D88050
TEXT_LO, TEXT_HI = 0x140001000, 0x14291D000  # .text from the PE header

os.makedirs(OUT_DIR, exist_ok=True)
summary = []


def log(s):
    summary.append(s)
    print(s)


def is_code_ptr(v):
    return TEXT_LO <= v < TEXT_HI


def ensure_func(ea):
    f = ida_funcs.get_func(ea)
    if f is None:
        ida_auto.auto_make_proc(ea)
        ida_auto.auto_wait()
        f = ida_funcs.get_func(ea)
        if f is None and ida_funcs.add_func(ea):
            f = ida_funcs.get_func(ea)
    return f


def decompile(ea, label):
    f = ensure_func(ea)
    if f is None:
        log(f"{label} {ea:#x}: could not create function")
        return
    try:
        cf = ida_hexrays.decompile(f.start_ea)
        text = str(cf)
    except Exception as e:  # noqa: BLE001
        text = f"// decompile failed: {e}\n"
    path = os.path.join(OUT_DIR, f"{label}_{f.start_ea:x}.c")
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(f"// {label} at {f.start_ea:#x} (rva {f.start_ea - 0x140000000:#x})\n")
        fh.write(text)
    log(f"{label} {f.start_ea:#x} -> {os.path.basename(path)} ({len(text)} chars)")


def table(ea, label, max_entries=40):
    for i in range(max_entries):
        v = ida_bytes.get_qword(ea + 8 * i)
        if not is_code_ptr(v):
            log(f"{label}: {i} entries")
            return
        decompile(v, f"{label}{i:02d}")


ida_hexrays.init_hexrays_plugin()
log(f"vftable name: {ida_name.get_name(VFTABLE)!r}")
table(VFTABLE, "vft")
table(STEPPERS, "step")

# Functions referencing the vftable: constructors / destructors.
seen = set()
for x in [VFTABLE]:
    ref = ida_xref.get_first_dref_to(x)
    while ref != idc.BADADDR:
        f = ensure_func(ref)
        if f and f.start_ea not in seen:
            seen.add(f.start_ea)
            decompile(f.start_ea, "xref")
        ref = ida_xref.get_next_dref_to(x, ref)
log(f"xref functions: {len(seen)}")

with open(os.path.join(OUT_DIR, "summary.txt"), "w", encoding="utf-8") as fh:
    fh.write("\n".join(summary))
ida_pro.qexit(0)
