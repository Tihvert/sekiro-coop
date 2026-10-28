"""Build crates/coop-dll/assets/icons_bc7.bin: pre-encoded BC7 blocks for the three co-op item
icon cells, which the DLL (icons.rs) writes into the menu atlas textures in memory while the game
loads menu:/{hi,low}/01_common.tpf. No game file is replaced (a me3 file override of
01_common.tpf.dcx crashed the game, 2026-10-23).

usage (regenerate after changing a design):
  py -3.14 tools\\icons\\build_icon_blob.py [host.png join.png leave.png] [--ids 251,552,962]
  defaults: tools\\icons\\design\\gpt-bell.png gpt-charm.png gpt-thread.png -> ids 251 552 962
then rebuild the DLL (build-dist.cmd); include_bytes! embeds the file.

Format (little endian):
  header: b"SCIC", u32 version = 1, u32 record count
  record: char[32] texture name (UTF-8, zero padded, e.g. "SB_Icon")
          u32 texture width, u32 texture height, u32 expected DDS byte size (148 + w*h for BC7)
          u32 block x, u32 block y, u32 blocks wide, u32 blocks high (4x4 BC7 blocks)
          u32 icon id
          u64 FNV-1a 64 of the vanilla block bytes (row by row), u64 FNV-1a 64 of the new bytes
          bw*bh*16 bytes of BC7 blocks, row-major
The DLL patches a cell only when the texture name, size, DXGI format (98 = BC7_UNORM), mip count
(1) and the vanilla hash all match; otherwise it logs and leaves the texture alone.
"""
import argparse
import io
import os
import struct
import sys

import numpy as np
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import bc7  # noqa: E402
import bhd5  # noqa: E402
import dcx  # noqa: E402
from build_icons import DEFAULT_IDS, layout_rects, prepare_icon  # noqa: E402
from souls import TPF, dds_info  # noqa: E402

REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
OUT = os.path.join(REPO, "crates", "coop-dll", "assets", "icons_bc7.bin")
DESIGN = os.path.join(HERE, "design")
DDS_HEADER = 148


def fnv1a64(data: bytes) -> int:
    h = 0xCBF29CE484222325
    for b in data:
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def block_rows(dds: bytes, w: int, bx: int, by: int, bw: int, bh: int) -> bytes:
    out = bytearray()
    for r in range(bh):
        o = DDS_HEADER + ((by + r) * (w // 4) + bx) * 16
        out += dds[o:o + bw * 16]
    return bytes(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("pngs", nargs="*", default=[os.path.join(DESIGN, f"gpt-{n}.png") for n in ("bell", "charm", "thread")])
    ap.add_argument("--ids", default=",".join(map(str, DEFAULT_IDS)))
    ap.add_argument("--out", default=OUT)
    ap.add_argument("--preview", default=r"F:\Temp\icons\preview_blob")
    args = ap.parse_args()
    ids = [int(v) for v in args.ids.split(",")]
    assert len(args.pngs) == 3 and len(ids) == 3
    os.makedirs(args.preview, exist_ok=True)

    records = []
    for res in ("hi", "low"):
        rects = layout_rects(res)
        payload, _ = dcx.decompress(bhd5.read_game_file(f"/menu/{res}/01_common.tpf.dcx"))
        tex = {t.name: t for t in TPF(payload).textures}
        for png, icon_id in zip(args.pngs, ids):
            image, x, y, cw, ch = rects[icon_id]
            t = tex[image]
            w, h, mips, fourcc, dxgi, _ = dds_info(t.dds)
            assert (fourcc, dxgi, mips, t.format) == (b"DX10", 98, 1, 102), (image, fourcc, dxgi, mips, t.format)
            assert len(t.dds) == DDS_HEADER + w * h, (image, len(t.dds))
            img = np.array(Image.open(io.BytesIO(t.dds)).convert("RGBA"))
            img[y:y + ch, x:x + cw] = prepare_icon(png, cw)
            bx, by = x // 4, y // 4
            bw, bh = (x + cw + 3) // 4 - bx, (y + ch + 3) // 4 - by
            px = img[by * 4:(by + bh) * 4, bx * 4:(bx + bw) * 4]
            new = bc7.encode(np.ascontiguousarray(px))
            old = block_rows(t.dds, w, bx, by, bw, bh)
            assert len(new) == len(old) == bw * bh * 16
            # preview: decode exactly what the DLL will write
            dec = np.array(Image.open(io.BytesIO(bc7.dds_header_bc7(bw * 4, bh * 4) + new)).convert("RGBA"))
            Image.fromarray(dec).save(os.path.join(args.preview, f"{res}_{icon_id}.png"))
            records.append((image, w, h, len(t.dds), bx, by, bw, bh, icon_id, fnv1a64(old), fnv1a64(new), new))
            print(f"[{res}] MENU_ItemIcon_{icon_id:05d}: {image} {w}x{h} cell ({x},{y}) {cw}x{ch} "
                  f"-> blocks ({bx},{by}) {bw}x{bh}, vanilla {fnv1a64(old):016x} new {fnv1a64(new):016x}")

    blob = bytearray(b"SCIC" + struct.pack("<II", 1, len(records)))
    for image, w, h, size, bx, by, bw, bh, icon_id, ho, hn, data in records:
        blob += image.encode().ljust(32, b"\0")
        blob += struct.pack("<IIIIIIIIQQ", w, h, size, bx, by, bw, bh, icon_id, ho, hn)
        blob += data
    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "wb") as f:
        f.write(blob)
    print(f"wrote {args.out} ({len(blob)} bytes, {len(records)} cells)")
    print("items.rs icon ids: HOST_ICON = %d, JOIN_ICON = %d, LEAVE_ICON = %d" % tuple(ids))


if __name__ == "__main__":
    main()
