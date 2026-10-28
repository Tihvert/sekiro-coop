"""List MENU_ItemIcon ids that exist in the vanilla atlases/layouts (hi + low) and in the
Scaleform menus, but are referenced by no param row (EquipParamGoods/Weapon/Protector/
Accessory, Magic, GemGen, CharMakeMenuListItem icon fields).

usage: py -3.14 find_unused_icons.py      (reads the game archives, writes nothing to the game)
Prints candidate ids; build_icons.py uses the ids chosen from this list.
"""
import os
import re
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import bhd5  # noqa: E402
import dcx  # noqa: E402
import gfx  # noqa: E402
from souls import BND4  # noqa: E402

GENERATED = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "vendor", "sekiro", "src",
                         "param", "generated.rs")
# param file -> struct name in vendor generated.rs
PARAM_STRUCTS = {
    "EquipParamGoods": "EQUIP_PARAM_GOODS_ST",
    "EquipParamWeapon": "EQUIP_PARAM_WEAPON_ST",
    "EquipParamProtector": "EQUIP_PARAM_PROTECTOR_ST",
    "EquipParamAccessory": "EQUIP_PARAM_ACCESSORY_ST",
    "Magic": "MAGIC_PARAM_ST",
    "GemGenParam": "GEM_GEN_PARAM_ST",
    "CharMakeMenuListItemParam": "CHARMAKEMENU_LISTITEM_PARAM_ST",
    "SpEffectParam": "SP_EFFECT_PARAM_ST",
    "MenuPropertySpecParam": "MENUPROPERTY_SPEC",
}
SIZES = {"i8": 1, "u8": 1, "i16": 2, "u16": 2, "i32": 4, "u32": 4, "f32": 4, "i64": 8, "u64": 8, "f64": 8}
FMT = {"i8": "b", "u8": "B", "i16": "h", "u16": "H", "i32": "i", "u32": "I"}

# Scaleform movies that show item icons (list from the SDT "adding a new icon" tutorial)
ICON_GFX = ["01_000_fe", "01_012_itemquantityselect", "02_200_mainmenu", "02_202_equipweaponselect",
            "02_280_getitem", "02_281_getitem_middle", "02_282_getitem_high", "02_284_getitem_event",
            "03_001_shop", "03_202_bonfire", "03_204_upgrade", "03_206_skill"]


def _fields(name):
    src = open(GENERATED, encoding="utf-8").read()
    m = re.search(r"pub struct %s \{(.*?)\n\}" % name, src, re.S)
    off = 0
    for line in m.group(1).splitlines():
        fm = re.match(r"\s*(\w+): (\[?)(\w+)(?:; (\d+)\])?,", line)
        if not fm:
            continue
        fname, arr, ty, n = fm.groups()
        al = SIZES[ty]
        off = (off + al - 1) // al * al
        yield fname, off, ty, bool(arr)
        off += SIZES[ty] * (int(n) if arr else 1)
    yield None, off, None, False


def struct_size(name):
    return list(_fields(name))[-1][1]


def struct_icon_fields(name):
    return [(f, o, t) for f, o, t, arr in _fields(name) if f and "icon_id" in f and not arr]


def param_rows(data):
    (count,) = struct.unpack_from("<H", data, 0x0A)
    rows = []
    for i in range(count):
        rid, doff = struct.unpack_from("<qq", data, 0x40 + 24 * i)
        rows.append((rid, doff))
    return rows


def used_icon_ids():
    raw, _ = dcx.decompress(bhd5.read_game_file("/param/gameparam/gameparam.parambnd.dcx"))
    bnd = BND4(raw)
    used = {}
    for f in bnd.files:
        pname = f.name.split("\\")[-1][:-6]
        if pname not in PARAM_STRUCTS:
            continue
        fields = struct_icon_fields(PARAM_STRUCTS[pname])
        rows = param_rows(f.data)
        if len(rows) > 1:  # sanity: struct size from generated.rs == row stride in the file
            stride = rows[1][1] - rows[0][1]
            assert stride == struct_size(PARAM_STRUCTS[pname]), (pname, stride, struct_size(PARAM_STRUCTS[pname]))
        for rid, doff in rows:
            for fname, off, ty in fields:
                (v,) = struct.unpack_from("<" + FMT[ty], f.data, doff + off)
                used.setdefault(v, []).append(f"{pname}[{rid}].{fname}")
    return used


def layout_ids(res):
    raw, _ = dcx.decompress(bhd5.read_game_file(f"/menu/{res}/01_common.sblytbnd.dcx"))
    out = {}
    for f in BND4(raw).files:
        s = f.data.decode("utf-8-sig")
        lay = f.name.split("\\")[-1]
        for n, x, y, w, h in re.findall(
                r'<SubTexture name="MENU_ItemIcon_(\d+)\.png" x="(\d+)" y="(\d+)" width="(\d+)" height="(\d+)"', s):
            out[int(n)] = (lay, int(x), int(y), int(w), int(h))
    return out


def gfx_ids():
    per = {}
    for g in ICON_GFX:
        data = bhd5.read_game_file(f"/menu/{g}.gfx")
        per[g] = gfx.item_icon_ids(data)
    return per


def main():
    used = used_icon_ids()
    hi, low = layout_ids("hi"), layout_ids("low")
    per_gfx = gfx_ids()
    in_all_gfx = set.intersection(*per_gfx.values())
    print("param-referenced icon ids:", len(used))
    print("layout ids hi/low:", len(hi), len(low))
    for g, s in per_gfx.items():
        print(f"  gfx {g}: {len(s)} MENU_ItemIcon exports")
    cands = sorted(i for i in hi if i in low and i in in_all_gfx and i not in used and hi[i][3] == 252)
    print("unused candidates (in hi+low layout, in every icon gfx, full-size, no param ref):")
    for i in cands:
        print(f"  {i:5d}  hi {hi[i]}  low {low[i]}")
    return cands


if __name__ == "__main__":
    main()
