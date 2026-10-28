"""Minimal BC7 encoder (mode 6 only), numpy-vectorized. Own implementation.

Mode 6: 1 subset, RGBA endpoints 7 bits + 1 unique p-bit per endpoint, 4-bit indices.
Bit layout (LSB first): mode '0000001' (7 bits), R0 R1 G0 G1 B0 B1 A0 A1 (7 bits each),
P0 P1, then 16 indices (first/anchor index 3 bits with implicit MSB 0, others 4 bits).
Quality is fine for menu icons (one subset, PCA endpoints + 2 least-squares refinements).
"""
import numpy as np

W4 = np.array([0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64], dtype=np.float64)


def _quant_endpoint(v):
    """v: (n,4) float endpoints -> (q7 (n,4) int, p (n,) int, value (n,4) float)"""
    best = None
    for p in (0, 1):
        q = np.clip(np.rint((v - p) / 2.0), 0, 127)
        val = q * 2 + p
        err = ((val - v) ** 2).sum(1)
        if best is None:
            best = [q, np.full(len(v), p), val, err]
        else:
            m = err < best[3]
            best[0][m] = q[m]; best[1][m] = p; best[2][m] = val[m]; best[3][m] = err[m]
    return best[0].astype(np.int64), best[1].astype(np.int64), best[2]


def _palette(v0, v1):
    # (n,16,4) palette using the BC7 interpolation ((64-w)*e0 + w*e1 + 32) >> 6
    w = W4[None, :, None]
    return np.floor(((64 - w) * v0[:, None, :] + w * v1[:, None, :] + 32) / 64.0)


def _encode_blocks(px):
    """px: (n,16,4) float 0..255 -> (n,16) uint8 bytes"""
    n = px.shape[0]
    mean = px.mean(1)
    c = px - mean[:, None, :]
    cov = np.einsum("nki,nkj->nij", c, c)
    axis = np.ones((n, 4)) / 2.0
    for _ in range(8):
        axis = np.einsum("nij,nj->ni", cov, axis)
        norm = np.linalg.norm(axis, axis=1, keepdims=True)
        axis = np.where(norm > 1e-9, axis / np.maximum(norm, 1e-12), 0.5)
    proj = np.einsum("nki,ni->nk", c, axis)
    e0 = np.clip(mean + proj.min(1)[:, None] * axis, 0, 255)
    e1 = np.clip(mean + proj.max(1)[:, None] * axis, 0, 255)

    for it in range(3):
        q0, p0, v0 = _quant_endpoint(e0)
        q1, p1, v1 = _quant_endpoint(e1)
        pal = _palette(v0, v1)
        d = ((px[:, :, None, :] - pal[:, None, :, :]) ** 2).sum(3)  # (n,16,16)
        idx = d.argmin(2)
        if it == 2:
            break
        # least-squares refinement of endpoints given the weights
        t = W4[idx] / 64.0  # (n,16)
        a = 1 - t
        aa = (a * a).sum(1); bb = (t * t).sum(1); ab = (a * t).sum(1)
        ax = np.einsum("nk,nkc->nc", a, px); bx = np.einsum("nk,nkc->nc", t, px)
        det = aa * bb - ab * ab
        ok = np.abs(det) > 1e-6
        detc = np.where(ok, det, 1.0)[:, None]
        ne0 = (bb[:, None] * ax - ab[:, None] * bx) / detc
        ne1 = (aa[:, None] * bx - ab[:, None] * ax) / detc
        e0 = np.where(ok[:, None], np.clip(ne0, 0, 255), e0)
        e1 = np.where(ok[:, None], np.clip(ne1, 0, 255), e1)

    # anchor fix: index 0 must have MSB 0
    flip = idx[:, 0] >= 8
    q0f, q1f = q0.copy(), q1.copy()
    q0f[flip], q1f[flip] = q1[flip], q0[flip]
    p0f, p1f = p0.copy(), p1.copy()
    p0f[flip], p1f[flip] = p1[flip], p0[flip]
    idx = np.where(flip[:, None], 15 - idx, idx)

    lo = np.zeros(n, dtype=np.uint64)
    hi = np.zeros(n, dtype=np.uint64)
    pos = 0

    def put(val, bits):
        nonlocal pos, lo, hi
        val = val.astype(np.uint64) & np.uint64((1 << bits) - 1)
        if pos + bits <= 64:
            lo |= val << np.uint64(pos)
        elif pos >= 64:
            hi |= val << np.uint64(pos - 64)
        else:
            low_bits = 64 - pos
            lo |= (val & np.uint64((1 << low_bits) - 1)) << np.uint64(pos)
            hi |= val >> np.uint64(low_bits)
        pos += bits

    put(np.full(n, 1 << 6), 7)  # mode 6
    for ch in range(4):
        put(q0f[:, ch], 7)
        put(q1f[:, ch], 7)
    put(p0f, 1)
    put(p1f, 1)
    put(idx[:, 0], 3)
    for k in range(1, 16):
        put(idx[:, k], 4)
    assert pos == 128
    out = np.empty((n, 16), dtype=np.uint8)
    out[:, :8] = lo.view(np.uint8).reshape(n, 8)
    out[:, 8:] = hi.view(np.uint8).reshape(n, 8)
    return out


def encode_blocks(blocks: np.ndarray) -> bytes:
    """blocks: (n,16,4) uint8, pixels row-major inside each 4x4 block -> n*16 bytes."""
    out = []
    for i in range(0, len(blocks), 8192):
        out.append(_encode_blocks(blocks[i:i + 8192].astype(np.float64)))
    return np.concatenate(out).tobytes()


def encode(rgba: np.ndarray) -> bytes:
    """rgba: (H,W,4) uint8, H and W multiples of 4 -> BC7 block data (row-major blocks)."""
    h, w, _ = rgba.shape
    assert h % 4 == 0 and w % 4 == 0
    blocks = rgba.reshape(h // 4, 4, w // 4, 4, 4).transpose(0, 2, 1, 3, 4).reshape(-1, 16, 4)
    out = []
    step = 8192
    for i in range(0, len(blocks), step):
        out.append(_encode_blocks(blocks[i:i + step].astype(np.float64)))
    return np.concatenate(out).tobytes()


def dds_header_bc7(w: int, h: int) -> bytes:
    """DDS + DX10 header identical in layout to the vanilla Sekiro menu textures (BC7_UNORM, 1 mip)."""
    import struct
    flags = 0x1 | 0x2 | 0x4 | 0x1000 | 0x80000  # CAPS|HEIGHT|WIDTH|PIXELFORMAT|LINEARSIZE
    hdr = struct.pack("<4sIIIIIII", b"DDS ", 124, flags, h, w, max(1, (w + 3) // 4) * max(1, (h + 3) // 4) * 16, 1, 1)  # depth 1, mips 1 (as vanilla)
    hdr += b"\0" * 44
    hdr += struct.pack("<II4sIIIII", 32, 0x4, b"DX10", 0, 0, 0, 0, 0)
    hdr += struct.pack("<IIIII", 0x1000, 0, 0, 0, 0)
    hdr += struct.pack("<IIIII", 98, 3, 0, 1, 0)  # BC7_UNORM, TEXTURE2D, misc, arraySize 1, misc2
    return hdr
