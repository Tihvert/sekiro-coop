"""BND4 and TPF (PC) read/write, own minimal implementation for Sekiro menu files.

BND4 (little endian, as used by Sekiro menu binders):
  0x00 "BND4" u8 unk04 u8 unk05 u8 0 u8 0 | u8 0 u8 bigEndian u8 !bitBigEndian u8 0
  0x0C i32 fileCount  0x10 i64 0x40  0x18 char[8] version  0x20 i64 fileHeaderSize
  0x28 i64 dataStart  0x30 u8 unicode u8 rawFormat u8 extended u8 0  0x34 i32 0
  0x38 i64 hashTableOffset (extended == 4)
  rawFormat bits are stored reversed (when bitBigEndian is false and bit0 clear).
  format bits: 0x02 IDs, 0x04 Names1, 0x08 Names2, 0x10 LongOffsets, 0x20 Compression
  file header: u8 flags, 3x0, i32 -1, i64 compSize, [i64 uncompSize if 0x20],
               u32/i64 dataOffset, [i32 id if 0x02], [u32 nameOffset if 0x0C]
  extended==4 hash table: i64 0, i32 bucketCount, u8 0x10 headerSize, u8 8 bucketSize, u8 8 entrySize, u8 0
     buckets (i32 count, i32 index) then entries (u32 hash, i32 fileIndex); hash = h*37+c of the name
     uppercase? -> see _bnd_hash (lower-case path with leading '/').
TPF (PC): "TPF\0" u32 dataSize i32 count u8 platform(0) u8 flag2 u8 encoding(1=UTF16) u8 0
  entries (20 bytes): u32 offset, i32 size, u8 format, u8 type, u8 mipmaps, u8 flags1,
                      u32 nameOffset, i32 flags2 (unk);  data is a whole DDS file.
"""
import struct


def _rev_bits(b):
    r = 0
    for i in range(8):
        if b & (1 << i):
            r |= 1 << (7 - i)
    return r


def _read_wstr(data, off):
    end = off
    while data[end:end + 2] != b"\0\0":
        end += 2
    return data[off:end].decode("utf-16-le")


def _read_cstr(data, off):
    end = data.index(b"\0", off)
    return data[off:end].decode("shift_jis", "replace")


def bnd_hash(name: str) -> int:
    # FromSoft binder hash: lower-case, '/' separators, leading '/', h = h*37 + c (u32)
    n = name.lower().replace("\\", "/")
    if not n.startswith("/"):
        n = "/" + n
    h = 0
    for c in n:
        h = (h * 37 + ord(c)) & 0xFFFFFFFF
    return h


class BndFile:
    def __init__(self, fid, name, data, flags=0x40):
        self.id, self.name, self.data, self.flags = fid, name, data, flags


class BND4:
    def __init__(self, data: bytes):
        assert data[:4] == b"BND4", data[:4]
        self.unk04, self.unk05 = data[4], data[5]
        self.big_endian = data[9]
        self.bit_big_endian = not data[10]
        assert not self.big_endian
        (count,) = struct.unpack_from("<i", data, 0x0C)
        self.version = data[0x18:0x20]
        (fh_size, data_start) = struct.unpack_from("<qq", data, 0x20)
        self.unicode, raw_fmt, self.extended = data[0x30], data[0x31], data[0x32]
        self.raw_format = raw_fmt
        rev = self.bit_big_endian or ((raw_fmt & 1) and not (raw_fmt & 0x80))
        fmt = raw_fmt if rev else _rev_bits(raw_fmt)
        self.format = fmt
        self.files = []
        p = 0x40
        for _ in range(count):
            flags = data[p]
            q = p + 8
            (csize,) = struct.unpack_from("<q", data, q); q += 8
            usize = csize
            if fmt & 0x20:
                (usize,) = struct.unpack_from("<q", data, q); q += 8
            if fmt & 0x10:
                (off,) = struct.unpack_from("<q", data, q); q += 8
            else:
                (off,) = struct.unpack_from("<I", data, q); q += 4
            fid = -1
            if fmt & 0x02:
                (fid,) = struct.unpack_from("<i", data, q); q += 4
            name = None
            if fmt & 0x0C:
                (noff,) = struct.unpack_from("<I", data, q); q += 4
                name = _read_wstr(data, noff) if self.unicode else _read_cstr(data, noff)
            assert q - p == fh_size, (q - p, fh_size)
            assert csize == usize, "compressed BND entries not supported"
            self.files.append(BndFile(fid, name, data[off:off + csize], flags))
            p = q

    def write(self) -> bytes:
        fmt = self.format
        fh = 1 + 3 + 4 + 8 + (8 if fmt & 0x20 else 0) + (8 if fmt & 0x10 else 4) + \
            (4 if fmt & 0x02 else 0) + (4 if fmt & 0x0C else 0)
        n = len(self.files)
        out = bytearray(0x40 + fh * n)
        # names
        name_offs = []
        for f in self.files:
            name_offs.append(len(out))
            out += (f.name.encode("utf-16-le") + b"\0\0") if self.unicode else (f.name.encode("shift_jis") + b"\0")
        hash_off = 0
        if self.extended == 4:
            while len(out) % 8:
                out += b"\0"
            hash_off = len(out)
            out += self._hash_table(hash_off)
        data_start = len(out)  # end of header data (unaligned), as in vanilla files
        data_offs = []
        for f in self.files:
            while len(out) % 16:
                out += b"\0"
            data_offs.append(len(out))
            out += f.data
        out[0:4] = b"BND4"
        out[4], out[5] = self.unk04, self.unk05
        out[9] = self.big_endian
        out[10] = 0 if self.bit_big_endian else 1
        struct.pack_into("<i", out, 0x0C, n)
        struct.pack_into("<q", out, 0x10, 0x40)
        out[0x18:0x20] = self.version
        struct.pack_into("<qq", out, 0x20, fh, data_start)
        out[0x30], out[0x31], out[0x32] = self.unicode, self.raw_format, self.extended
        struct.pack_into("<q", out, 0x38, hash_off)
        p = 0x40
        for i, f in enumerate(self.files):
            out[p] = f.flags
            struct.pack_into("<i", out, p + 4, -1)
            q = p + 8
            struct.pack_into("<q", out, q, len(f.data)); q += 8
            if fmt & 0x20:
                struct.pack_into("<q", out, q, len(f.data)); q += 8
            if fmt & 0x10:
                struct.pack_into("<q", out, q, data_offs[i]); q += 8
            else:
                struct.pack_into("<I", out, q, data_offs[i]); q += 4
            if fmt & 0x02:
                struct.pack_into("<i", out, q, f.id); q += 4
            if fmt & 0x0C:
                struct.pack_into("<I", out, q, name_offs[i]); q += 4
            p = q
        return bytes(out)

    def _hash_table(self, base: int) -> bytes:
        # Bucket count: smallest prime >= count/7 style (SoulsFormats uses a prime search);
        # the game only needs consistency between buckets and hashes.
        n = len(self.files)
        bc = max(1, n // 7)
        while True:
            if bc > 1 and all(bc % d for d in range(2, int(bc ** 0.5) + 1)):
                break
            if bc <= 1:
                bc = 2 if n else 1
                break
            bc += 1
        buckets = [[] for _ in range(bc)]
        for i, f in enumerate(self.files):
            h = bnd_hash(f.name)
            buckets[h % bc].append((h, i))
        # i64 = absolute offset of the entry array (after the bucket array)
        hdr = struct.pack("<qiBBBB", base + 16 + 8 * bc, bc, 0x10, 8, 8, 0)
        b = bytearray()
        e = bytearray()
        idx = 0
        for bk in buckets:
            bk.sort()
            b += struct.pack("<ii", len(bk), idx)
            for h, i in bk:
                e += struct.pack("<Ii", h, i)
            idx += len(bk)
        return hdr + bytes(b) + bytes(e)


class Texture:
    def __init__(self, name, fmt, ttype, mips, flags1, flags2, dds):
        self.name, self.format, self.type, self.mipmaps = name, fmt, ttype, mips
        self.flags1, self.flags2, self.dds = flags1, flags2, dds


class TPF:
    def __init__(self, data: bytes = None):
        self.textures = []
        self.platform, self.flag2, self.encoding = 0, 3, 1
        if data is None:
            return
        assert data[:4] == b"TPF\0"
        (count,) = struct.unpack_from("<i", data, 8)
        self.platform, self.flag2, self.encoding = data[12], data[13], data[14]
        assert self.platform == 0, "only PC TPF supported"
        p = 16
        for _ in range(count):
            off, size, fmt, ttype, mips, fl1, noff, fl2 = struct.unpack_from("<IiBBBBIi", data, p)
            assert fl1 in (0, 1), f"flags1={fl1} unsupported"
            p += 20
            name = _read_wstr(data, noff) if self.encoding == 1 else _read_cstr(data, noff)
            self.textures.append(Texture(name, fmt, ttype, mips, fl1, fl2, data[off:off + size]))

    def write(self) -> bytes:
        n = len(self.textures)
        out = bytearray(16 + 20 * n)
        noffs = []
        for t in self.textures:
            noffs.append(len(out))
            out += (t.name.encode("utf-16-le") + b"\0\0") if self.encoding == 1 else (t.name.encode("shift_jis") + b"\0")
        data_start = len(out)  # vanilla menu TPFs: no alignment padding anywhere
        doffs = []
        for t in self.textures:
            doffs.append(len(out))
            out += t.dds
        out[0:4] = b"TPF\0"
        struct.pack_into("<Ii", out, 4, len(out) - data_start, n)
        out[12], out[13], out[14] = self.platform, self.flag2, self.encoding
        for i, t in enumerate(self.textures):
            struct.pack_into("<IiBBBBIi", out, 16 + 20 * i, doffs[i], len(t.dds), t.format, t.type,
                             t.mipmaps, t.flags1, noffs[i], t.flags2)
        return bytes(out)


def dds_info(dds: bytes):
    """(width, height, mips, fourcc_or_dxgi, pixel_flags)"""
    assert dds[:4] == b"DDS "
    h, w = struct.unpack_from("<II", dds, 12)
    (mips,) = struct.unpack_from("<I", dds, 28)
    pf_flags = struct.unpack_from("<I", dds, 80)[0]
    fourcc = dds[84:88]
    extra = None
    if fourcc == b"DX10":
        (extra,) = struct.unpack_from("<I", dds, 128)
    return w, h, mips, fourcc, extra, pf_flags
