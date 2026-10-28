"""Read-only Scaleform GFX (SWF-like) scanner: which MENU_ItemIcon_<id> external images a
movie defines, and in which sprite/frame each one is placed. Own implementation.

GFX header: "GFX" u8 version, u32 length, RECT (bit packed), u16 frameRate, u16 frameCount, tags.
Tag header: u16 (code << 6 | len), len == 0x3F -> u32 len.
  1009 DefineExternalImage2: u32 charId, u16 fmt, u16 w, u16 h, u8 n + exportName, u8 n + fileName ...
  39 DefineSprite: u16 id, u16 frames, nested tags;  1 ShowFrame
  26 PlaceObject2: u8 flags, u16 depth, [u16 charId if flags & 2]
  70 PlaceObject3: u8 flags, u8 flags2, u16 depth, [className if flags2 & 8],
                   [u16 charId if flags & 2]
"""
import re
import struct


def _tags(data, p, end):
    while p < end:
        (h,) = struct.unpack_from("<H", data, p)
        p += 2
        code, ln = h >> 6, h & 0x3F
        if ln == 0x3F:
            (ln,) = struct.unpack_from("<I", data, p)
            p += 4
        yield code, p, ln
        p += ln
        if code == 0:
            return


def _body_start(data):
    assert data[:3] in (b"GFX", b"FWS"), data[:3]
    nbits = data[8] >> 3
    rect_bytes = (5 + 4 * nbits + 7) // 8
    return 8 + rect_bytes + 4


def scan(data):
    """-> (images {charId: exportName}, placements {charId: [(spriteId, frameIndex)]})"""
    images = {}
    places = {}

    def walk(p, end, sprite):
        frame = 0
        for code, q, ln in _tags(data, p, end):
            body = data[q:q + ln]
            if code == 1009:
                (cid,) = struct.unpack_from("<I", body, 0)
                n = body[10]
                images[cid] = body[11:11 + n].decode("latin-1")
            elif code == 39:
                sid, _ = struct.unpack_from("<HH", body, 0)
                walk(q + 4, q + ln, sid)
            elif code == 1:
                frame += 1
            elif code == 26:
                if body[0] & 2:
                    (cid,) = struct.unpack_from("<H", body, 3)
                    places.setdefault(cid, []).append((sprite, frame))
            elif code == 70:
                fl, fl2 = body[0], body[1]
                r = 4
                if fl2 & 0x08:  # (HasImage+HasCharacter does not imply a class name in these files)
                    r = body.index(b"\0", r) + 1
                if fl & 2:
                    (cid,) = struct.unpack_from("<H", body, r)
                    places.setdefault(cid, []).append((sprite, frame))

    walk(_body_start(data), len(data), None)
    return images, places


def item_icon_ids(data, placed_only=True):
    images, places = scan(data)
    out = set()
    for cid, name in images.items():
        m = re.fullmatch(r"MENU_ItemIcon_(\d+)", name)
        if m and (not placed_only or cid in places):
            out.add(int(m.group(1)))
    return out


if __name__ == "__main__":
    import sys
    d = open(sys.argv[1], "rb").read()
    imgs, pl = scan(d)
    for cid, name in sorted(imgs.items(), key=lambda kv: kv[1]):
        if "ItemIcon" in name:
            print(cid, name, pl.get(cid))
