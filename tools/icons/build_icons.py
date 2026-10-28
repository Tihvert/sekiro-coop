"""SUPERSEDED (2026-10-23): the me3 file override this builds crashed the game live
(sekiro.exe+0x71B2E9). The icons are now patched in memory by the DLL (icons.rs); generate its
data with build_icon_blob.py. Kept for layout_rects / prepare_icon and as a reference.

Build the me3 asset package with the three co-op item icons.

usage:
  py -3.14 build_icons.py <host.png> <join.png> <leave.png> [--out DIR] [--ids 251,552,962]
                          [--level 4] [--preview DIR]

What it does (Sekiro 1.06):
  * Item icons are sub-rectangles of BC7 atlas textures in menu/{hi,low}/01_common.tpf.dcx,
    placed by menu/{hi,low}/01_common.sblytbnd.dcx (SB_Icon*.layout, "MENU_ItemIcon_%05d.png").
  * The Scaleform menus (*.gfx) only show an icon id that has its own DefineExternalImage2 +
    sprite frame (frame = id % 1000), so brand-new ids would need ~12 GFX edits. Instead we
    repaint three vanilla icon cells that no param row references (see find_unused_icons.py).
  * Only the 4x4 BC7 blocks under those three cells are re-encoded; every other byte of the
    vanilla atlas is copied unchanged. Layouts and GFX stay vanilla.
  * Output (me3 package dir, game-relative layout):
        <out>/menu/hi/01_common.tpf.dcx
        <out>/menu/low/01_common.tpf.dcx
Reads the original files from the game archives every run (idempotent); never writes to the game.
"""
import argparse
import io
import os
import re
import sys

import numpy as np
from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import bc7  # noqa: E402
import bhd5  # noqa: E402
import dcx  # noqa: E402
from souls import BND4, TPF, dds_info  # noqa: E402

REPO = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
DEFAULT_OUT = os.path.join(REPO, "dist", "sekiro-coop", "icons")
# Vanilla icon ids that exist in hi+low layouts and every icon GFX but are referenced by no param
# row (find_unused_icons.py). 251: glowing blade, 552: droplet variant, 962: scroll variant.
DEFAULT_IDS = (251, 552, 962)
NAMES = ("host", "join", "leave")
DDS_HEADER = 148  # "DDS " + 124-byte header + 20-byte DX10 header


def layout_rects(res):
    raw, _ = dcx.decompress(bhd5.read_game_file(f"/menu/{res}/01_common.sblytbnd.dcx"))
    rects = {}
    for f in BND4(raw).files:
        s = f.data.decode("utf-8-sig")
        image = re.search(r'imagePath="([^"]+)\.png"', s).group(1)
        for n, x, y, w, h in re.findall(
                r'<SubTexture name="MENU_ItemIcon_(\d+)\.png" x="(\d+)" y="(\d+)" width="(\d+)" height="(\d+)"', s):
            rects[int(n)] = (image, int(x), int(y), int(w), int(h))
    return rects


def prepare_icon(path, size):
    """Load a PNG, pad to square (centered), resize to size x size with premultiplied-alpha Lanczos."""
    im = Image.open(path).convert("RGBA")
    w, h = im.size
    if w != h:
        s = max(w, h)
        canvas = Image.new("RGBA", (s, s), (0, 0, 0, 0))
        canvas.paste(im, ((s - w) // 2, (s - h) // 2))
        im = canvas
    a = np.asarray(im).astype(np.float32) / 255.0
    pm = a.copy()
    pm[..., :3] *= pm[..., 3:4]
    chans = [Image.fromarray(pm[..., c], mode="F").resize((size, size), Image.LANCZOS) for c in range(4)]
    out = np.stack([np.asarray(c) for c in chans], -1)
    out = np.clip(out, 0.0, 1.0)
    alpha = out[..., 3:4]
    rgb = np.where(alpha > 1e-4, out[..., :3] / np.maximum(alpha, 1e-4), 0.0)
    res = np.concatenate([np.clip(rgb, 0, 1), alpha], -1)
    res = np.rint(res * 255).astype(np.uint8)
    res[res[..., 3] == 0] = 0  # vanilla: fully transparent pixels are (0,0,0,0)
    return res


def patch_atlas(dds: bytes, edits):
    """edits: list of (x, y, w, h, rgba). Returns (new_dds, before_rgba, after_rgba)."""
    w, h, mips, fourcc, dxgi, _ = dds_info(dds)
    assert fourcc == b"DX10" and dxgi == 98 and mips == 1, (fourcc, dxgi, mips)
    img = np.array(Image.open(io.BytesIO(dds)).convert("RGBA"))
    before = img.copy()
    blocks = set()
    for x, y, cw, ch, rgba in edits:
        img[y:y + ch, x:x + cw] = rgba
        for by in range(y // 4, (y + ch + 3) // 4):
            for bx in range(x // 4, (x + cw + 3) // 4):
                blocks.add((by, bx))
    data = bytearray(dds)
    bw = w // 4
    order = sorted(blocks)
    px = np.stack([img[by * 4:by * 4 + 4, bx * 4:bx * 4 + 4] for by, bx in order])  # (n,4,4,4)
    enc = np.frombuffer(bc7.encode_blocks(px.reshape(-1, 16, 4)), dtype=np.uint8).reshape(-1, 16)
    for (by, bx), blk in zip(order, enc):
        o = DDS_HEADER + (by * bw + bx) * 16
        data[o:o + 16] = blk.tobytes()
    after = np.array(Image.open(io.BytesIO(bytes(data))).convert("RGBA"))
    return bytes(data), before, after, img, order


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("host")
    ap.add_argument("join")
    ap.add_argument("leave")
    ap.add_argument("--out", default=DEFAULT_OUT)
    ap.add_argument("--ids", default=",".join(map(str, DEFAULT_IDS)))
    ap.add_argument("--level", type=int, default=4, help="Kraken level (vanilla header says 6; 4 is much faster)")
    ap.add_argument("--preview", default=r"F:\Temp\icons\preview")
    args = ap.parse_args()
    ids = [int(v) for v in args.ids.split(",")]
    assert len(ids) == 3
    pngs = [args.host, args.join, args.leave]
    os.makedirs(args.preview, exist_ok=True)

    for res in ("hi", "low"):
        rects = layout_rects(res)
        raw_dcx = bhd5.read_game_file(f"/menu/{res}/01_common.tpf.dcx")
        payload, info = dcx.decompress(raw_dcx)
        tpf = TPF(payload)
        tex = {t.name: t for t in tpf.textures}
        per_atlas = {}
        for name, png, icon_id in zip(NAMES, pngs, ids):
            image, x, y, w, h = rects[icon_id]
            assert w == h, (icon_id, w, h)
            per_atlas.setdefault(image, []).append((x, y, w, h, prepare_icon(png, w)))
            print(f"[{res}] {name:5s} -> MENU_ItemIcon_{icon_id:05d} in {image} at ({x},{y}) {w}x{h}")
        for image, edits in per_atlas.items():
            t = tex[image]
            new, before, after, want, blocks = patch_atlas(t.dds, edits)
            # verification: untouched pixels are bit-identical, edited cells match the intended icon
            mask = np.zeros(before.shape[:2], bool)
            for by, bx in blocks:
                mask[by * 4:by * 4 + 4, bx * 4:bx * 4 + 4] = True
            assert np.array_equal(before[~mask], after[~mask]), "pixels outside the edited blocks changed"
            err = ((after[mask].astype(np.float64) - want[mask]) ** 2).mean()
            psnr = 10 * np.log10(255 ** 2 / max(err, 1e-9))
            print(f"[{res}] {image}: re-encoded {len(blocks)} BC7 blocks, PSNR {psnr:.1f} dB, rest unchanged")
            t.dds = new
            for x, y, w, h, _ in edits:
                Image.fromarray(after[y:y + h, x:x + w]).save(
                    os.path.join(args.preview, f"{res}_{image}_{x}_{y}.png"))
        out_bytes = dcx.compress(tpf.write(), info, args.level)
        # round-trip check of what we are about to ship
        chk, _ = dcx.decompress(out_bytes)
        vanilla = {t.name: t.dds for t in TPF(payload).textures}
        changed = [t.name for t in TPF(chk).textures if t.dds != vanilla[t.name]]
        assert sorted(changed) == sorted(per_atlas), changed
        assert [t.name for t in TPF(chk).textures] == list(vanilla)
        dst = os.path.join(args.out, "menu", res, "01_common.tpf.dcx")
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        with open(dst, "wb") as f:
            f.write(out_bytes)
        print(f"[{res}] wrote {dst} ({len(out_bytes)} bytes; vanilla {len(raw_dcx)})")

    print()
    print("EquipParamGoods icon_id values for the co-op items:")
    for name, icon_id in zip(NAMES, ids):
        print(f"  {name.upper()}_ICON = {icon_id}")


if __name__ == "__main__":
    main()
