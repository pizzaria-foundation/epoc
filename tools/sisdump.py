#!/usr/bin/env python3
"""Walk a SISX package and lay its insides bare.

Where sisextract.py brute-forces the payloads out (it only wants a reference
E32 to diff a header against), this parses the container properly: the
SISController tells us *where* every file installs and *what* the package
declares about itself, and the SISData holds the payloads keyed by the same
index the controller uses. That mapping — target path in hand, bytes in hand —
is the whole point when the goal is to understand how someone else's package
made itself a homescreen, a hidden app, or a startup entry.

    python3 tools/sisdump.py <file.sis> [outdir]

With an outdir the payloads are written there under their install basename;
without one it prints the manifest and stops. The format is the post-9.1 SISX
container (UID 0x10201A7A), which is what every S60 3rd edition device speaks.
"""

import os
import struct
import sys
import zlib

# SISField type tags. Only the ones we name are interesting; the rest print as
# their number so an unexpected field is visible rather than silently skipped.
FT = {
    1: "String", 2: "Array", 3: "Compressed", 4: "Version", 5: "VersionRange",
    6: "Date", 7: "Time", 8: "DateTime", 9: "Uid", 11: "Language",
    12: "Contents", 13: "Controller", 14: "Info", 15: "SupportedLanguages",
    16: "SupportedOptions", 17: "Prerequisites", 18: "Dependency",
    19: "Properties", 20: "Property", 21: "Signatures", 22: "CertChain",
    23: "Logo", 24: "FileDescription", 25: "Hash", 26: "If", 27: "ElseIf",
    28: "InstallBlock", 29: "Expression", 30: "Data", 31: "DataUnit",
    32: "FileData", 33: "SupportedOption", 34: "ControllerChecksum",
    35: "DataChecksum", 36: "Signature", 37: "Blob", 38: "SigAlgorithm",
    39: "SigCertChain", 40: "DataIndex", 41: "Capabilities",
}

SISX_UID = 0x10201A7A


def u32(b, o):
    return struct.unpack_from("<I", b, o)[0]


def pad4(o):
    return (o + 3) & ~3


def parse_field(buf, off):
    """A normal SISField: {u32 type, u32 length, data[length], pad to 4}."""
    t, l = struct.unpack_from("<II", buf, off)
    body = buf[off + 8:off + 8 + l]
    return t, body, pad4(off + 8 + l)


def array_elems(body):
    """A SISArray stores the element type once, then bare {u32 len, data} runs —
    the per-element type tag is omitted because the header already fixed it."""
    etype = u32(body, 0)
    off, out = 4, []
    while off + 4 <= len(body):
        l = u32(body, off)
        out.append(body[off + 4:off + 4 + l])
        off = pad4(off + 4 + l)
    return etype, out


def children(body):
    """Iterate the fields packed inside a compound field's body."""
    off = 0
    while off + 8 <= len(body):
        t, b, nxt = parse_field(body, off)
        yield t, b
        off = nxt


def sstr(body):
    return body.decode("utf-16le", "replace")


def decompress(comp_body):
    """A SISCompressed body is {u32 algorithm, u64 uncompressedSize, bytes}.
    Algorithm 0 is stored verbatim; 1 is deflate, written either zlib-wrapped
    or raw depending on the packager, so try both."""
    algo = u32(comp_body, 0)
    payload = comp_body[12:]
    if algo == 0:
        return payload
    try:
        return zlib.decompress(payload)
    except zlib.error:
        return zlib.decompressobj(-15).decompress(payload)


def parse_file_description(el):
    """The leading part is fields (Target String, MimeType String, optional
    Capabilities, Hash), but the tail is a *raw* struct that a field-walker
    misreads: {u32 operation, u32 operationOptions, u64 length,
    u64 uncompressedLength, u32 fileIndex}. Which optional fields appear varies
    between packagers, so don't count from the front — anchor on the end. The
    file index is always the last 4 bytes; the uncompressed length the 8 before;
    the compressed length the 8 before that. The target is the first field, which
    is unambiguous. That's everything the extraction needs."""
    _, target, _ = parse_field(el, 0)
    index = u32(el, len(el) - 4)
    clen, ulen = struct.unpack_from("<QQ", el, len(el) - 20)
    return sstr(target), index, clen, ulen


def collect_payloads(data_body):
    """Data -> Array of DataUnit -> Array of FileData -> Compressed. One flat
    list of decompressed payloads; a file description's index selects into it."""
    out = []
    _, arr, _ = parse_field(data_body, 0)
    _, units = array_elems(arr)
    for unit in units:
        _, farr, _ = parse_field(unit, 0)
        _, files = array_elems(farr)
        for fd in files:
            _, comp, _ = parse_field(fd, 0)
            out.append(decompress(comp))
    return out


def main():
    if len(sys.argv) < 2:
        sys.exit(f"usage: {sys.argv[0]} <file.sis> [outdir]")
    path = sys.argv[1]
    outdir = sys.argv[2] if len(sys.argv) > 2 else None
    data = open(path, "rb").read()

    uid1 = u32(data, 0)
    if uid1 != SISX_UID:
        print(f"warning: UID1 is 0x{uid1:08x}, not the SISX 0x{SISX_UID:08x} — "
              "this may be an old-style SIS or not a package at all")

    ctype, contents, _ = parse_field(data, 0x10)
    if FT.get(ctype) != "Contents":
        sys.exit(f"expected SISContents at 0x10, found type {ctype}")

    controller = payloads = None
    for t, body in children(contents):
        if FT.get(t) == "Compressed":
            controller = decompress(body)
        elif FT.get(t) == "Data":
            payloads = collect_payloads(body)
    if controller is None:
        sys.exit("no controller found")

    # The controller is a single SISController field wrapping everything.
    _, ctrl, _ = parse_field(controller, 0)

    name = vendor = uid = None
    deps = []
    files = []
    for t, body in children(ctrl):
        n = FT.get(t)
        if n == "Info":
            fs = list(children(body))
            for ft, fb in fs:
                if FT.get(ft) == "Uid":
                    uid = u32(fb, 0)
            # Info holds Uid, vendor-unique String, names Array, vendor Array.
            strings = [sstr(fb) for ft, fb in fs if FT.get(ft) == "String"]
            arrays = [fb for ft, fb in fs if FT.get(ft) == "Array"]
            if arrays:
                _, names = array_elems(arrays[0])
                if names:
                    name = sstr(names[0])
            if len(arrays) > 1:
                _, vends = array_elems(arrays[1])
                if vends:
                    vendor = sstr(vends[0])
        elif n == "Prerequisites":
            for at, ab in children(body):
                if FT.get(at) != "Array":
                    continue
                _, dulist = array_elems(ab)
                for dep in dulist:
                    dstrs = []
                    for dt, db in children(dep):
                        if FT.get(dt) == "Array":
                            _, ds = array_elems(db)
                            dstrs += [sstr(x) for x in ds]
                    if dstrs:
                        deps.append(dstrs[0])
        elif n == "InstallBlock":
            for at, ab in children(body):
                if FT.get(at) != "Array":
                    continue
                etype, elems = array_elems(ab)
                if FT.get(etype) == "FileDescription":
                    for el in elems:
                        files.append(parse_file_description(el))

    print(f"package : {name!r}")
    print(f"uid     : 0x{uid:08x}" if uid is not None else "uid     : ?")
    print(f"vendor  : {vendor!r}")
    if deps:
        print("requires:")
        for d in deps:
            print(f"    {d}")
    print(f"files   : {len(files)}")
    for target, index, clen, ulen in sorted(files, key=lambda f: f[1]):
        print(f"  [{index}] {target}   ({ulen} bytes)")

    if outdir is None:
        return 0
    if payloads is None:
        print("no data section — nothing to write")
        return 1
    os.makedirs(outdir, exist_ok=True)
    for target, index, clen, ulen in files:
        if index >= len(payloads):
            print(f"  ! index {index} past {len(payloads)} payloads, skipped")
            continue
        base = target.replace("\\", "/").rsplit("/", 1)[-1]
        with open(os.path.join(outdir, base), "wb") as f:
            f.write(payloads[index])
        print(f"  wrote {base}  ({len(payloads[index])} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
