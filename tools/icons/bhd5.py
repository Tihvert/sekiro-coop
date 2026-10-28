"""Minimal Sekiro BHD5/BDT reader (own implementation, stdlib + pycryptodome/cryptography).

Format notes (learned from public format docs; SoulsFormats/UXM read for reference only):
  * The .bhd header is RSA "raw" encrypted: each 256-byte block c -> m = c^e mod n,
    m written big-endian left-padded to 255 bytes.
  * BHD5 (DS3/Sekiro layout, little endian):
      0x00 "BHD5", 0x04 i8 endian(-1=LE) + 3 pad, 0x08 i32 1, 0x0C i32 file size,
      0x10 i32 bucket count, 0x14 i32 buckets offset, 0x18 i32 salt length, 0x1C salt.
    bucket: i32 count, i32 offset -> count file headers of 40 bytes:
      u32 hash, i32 padded size, i64 offset, i64 sha offset, i64 aes offset, i64 unpadded size
    aes: 16-byte key, i32 range count, ranges of (i64 start, i64 end) relative to the file;
      AES-128-ECB decrypt each range (start == -1 means unused).
  * Path hash (pre-Elden Ring): h = h*37 + c over the normalized path
    (lowercase, '/' separators, leading '/').
"""
import os
import struct
import sys

from cryptography.hazmat.primitives.serialization import load_pem_public_key

try:
    from Crypto.Cipher import AES  # pycryptodome
except ImportError:  # pragma: no cover
    AES = None

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from sekiro_keys import SEKIRO_KEYS  # noqa: E402

GAME_DIR = r"F:\SteamLibrary\steamapps\common\Sekiro"


def normalize(path: str) -> str:
    if ":" in path:
        path = path[path.index(":") + 1:]
    path = path.lower().replace("\\", "/").strip()
    if not path.startswith("/"):
        path = "/" + path
    return path


def path_hash(path: str) -> int:
    h = 0
    for c in normalize(path):
        h = (h * 37 + ord(c)) & 0xFFFFFFFF
    return h


def rsa_decrypt(data: bytes, pem: str) -> bytes:
    pub = load_pem_public_key(pem.encode()).public_numbers()
    n, e = pub.n, pub.e
    out = bytearray()
    for i in range(0, len(data), 256):
        c = int.from_bytes(data[i:i + 256], "big")
        out += pow(c, e, n).to_bytes(255, "big")
    return bytes(out)


class Entry:
    __slots__ = ("hash", "padded", "offset", "sha_off", "aes_off", "size", "aes")

    def __repr__(self):
        return f"Entry(hash={self.hash:08X} off={self.offset} size={self.size} padded={self.padded} aes={self.aes is not None})"


class Archive:
    def __init__(self, name: str, game_dir: str = GAME_DIR):
        self.name = name
        self.bdt_path = os.path.join(game_dir, name + ".bdt")
        raw = open(os.path.join(game_dir, name + ".bhd"), "rb").read()
        bhd = rsa_decrypt(raw, SEKIRO_KEYS[name])
        if bhd[:4] != b"BHD5":
            raise ValueError(f"{name}: bad BHD5 magic after decrypt: {bhd[:16].hex()}")
        self.bhd = bhd
        _, _, _, fsize, nb, boff, slen = struct.unpack_from("<4sb3xiiiii", bhd, 0)
        self.salt = bhd[0x1C:0x1C + slen].decode("ascii", "replace")
        self.entries = {}
        for b in range(nb):
            cnt, off = struct.unpack_from("<ii", bhd, boff + b * 8)
            for i in range(cnt):
                e = Entry()
                (e.hash, e.padded, e.offset, e.sha_off, e.aes_off, e.size) = struct.unpack_from(
                    "<Iiqqqq", bhd, off + i * 40)
                e.aes = None
                if e.aes_off:
                    key = bhd[e.aes_off:e.aes_off + 16]
                    (rc,) = struct.unpack_from("<i", bhd, e.aes_off + 16)
                    ranges = [struct.unpack_from("<qq", bhd, e.aes_off + 20 + 16 * r) for r in range(rc)]
                    e.aes = (key, ranges)
                self.entries[e.hash] = e

    def find(self, path: str):
        return self.entries.get(path_hash(path))

    def read(self, path_or_entry) -> bytes:
        e = path_or_entry if isinstance(path_or_entry, Entry) else self.find(path_or_entry)
        if e is None:
            raise KeyError(path_or_entry)
        with open(self.bdt_path, "rb") as f:
            f.seek(e.offset)
            data = bytearray(f.read(e.padded))
        if e.aes is not None:
            key, ranges = e.aes
            cipher = AES.new(key, AES.MODE_ECB)
            for s, t in ranges:
                if s == -1 or t <= s:
                    continue
                data[s:t] = cipher.decrypt(bytes(data[s:t]))
        size = e.size if e.size > 0 else e.padded
        return bytes(data[:size])


_ARCHIVES = {}


def archives():
    if not _ARCHIVES:
        for i in range(1, 6):
            _ARCHIVES[f"Data{i}"] = Archive(f"Data{i}")
    return _ARCHIVES


def locate(path: str):
    for a in archives().values():
        e = a.find(path)
        if e is not None:
            return a, e
    return None, None


def read_game_file(path: str) -> bytes:
    a, e = locate(path)
    if a is None:
        raise KeyError(path)
    return a.read(e)


if __name__ == "__main__":
    # usage: bhd5.py <virtual path> [<out file>]   (no args: summary)
    if len(sys.argv) == 1:
        for n, a in archives().items():
            print(n, "salt", a.salt, "entries", len(a.entries))
    else:
        a, e = locate(sys.argv[1])
        print(sys.argv[1], f"hash={path_hash(sys.argv[1]):08X}", a.name if a else None, e)
        if a and len(sys.argv) > 2:
            os.makedirs(os.path.dirname(os.path.abspath(sys.argv[2])), exist_ok=True)
            open(sys.argv[2], "wb").write(a.read(e))
