#!/usr/bin/env python3
"""Read a tcpdump/pcapng capture of Pro DJ Link dbserver traffic and dump
decoded messages (both wire variants: plain magic and tagged magic).

Usage: gtdump.py <capture.pcapng|pcap> [stdout]
"""
import sys, struct

MAGIC = b"\x87\x23\x49\xae"
# introduce-kind so INTRODUCE isn't mistaken for SUCCESS
NAME = {
    0x0000: "INTRODUCE", 0x0001: "MENU_CLOSE", 0x0100: "DISCONNECT",
    0x1000: "MENU_ROOT", 0x1001: "MENU_GENRE", 0x1002: "MENU_ARTIST",
    0x1003: "MENU_ALBUM", 0x1004: "MENU_TRACK", 0x1006: "MENU_BPM",
    0x1007: "MENU_RATING", 0x1008: "MENU_YEAR", 0x100a: "MENU_LABEL",
    0x100d: "MENU_COLOR", 0x1010: "MENU_TIME", 0x1011: "MENU_BITRATE",
    0x1012: "MENU_HISTORY", 0x1013: "MENU_FILENAME", 0x1014: "MENU_KEY",
    0x100f: "MENU_TAG_LIST", 0x1105: "MENU_PLAYLIST", 0x1300: "MENU_SEARCH",
    0x1400: "MENU_SORT", 0x1500: "MENU_1500", 0x2006: "MENU_FOLDER",
    0x2002: "GET_METADATA", 0x2003: "GET_ARTWORK", 0x2004: "GET_WAVEFORM_PREVIEW",
    0x2102: "GET_TRACK_INFO", 0x2104: "GET_CUE_POINTS", 0x2202: "GET_GENERIC_METADATA",
    0x2204: "GET_BEAT_GRID", 0x2504: "GET_VBR_INDEX", 0x2005: "HAND_BACK_PREVIEW",
    0x2205: "HAND_BACK_VBR", 0x2904: "GET_WAVEFORM_DETAIL", 0x2b04: "GET_CUE_POINTS_EXT",
    0x2c04: "GET_ANALYSIS_TAG", 0x2d04: "GET_ANALYSIS_TAG2", 0x3000: "RENDER_MENU",
    0x3001: "UNKNOWN_3001", 0x3002: "TAG_LIST_ADD", 0x3100: "UNKNOWN_3100",
    0x3202: "TAG_LIST_CLEAR", 0x3402: "UNKNOWN_3402", 0x3503: "UNPLAYABLE",
    0x3903: "GET_MEDIA_INFO", 0x3d03: "UNKNOWN_3D03", 0x3e03: "UNKNOWN_3E03",
    0x4000: "SUCCESS", 0x4001: "MENU_HEADER", 0x4002: "ARTWORK", 0x4003: "ERROR",
    0x4101: "MENU_ITEM", 0x4201: "MENU_FOOTER", 0x4402: "WAVEFORM_PREVIEW",
    0x4502: "VBR_INDEX", 0x4602: "BEAT_GRID", 0x4702: "CUE_POINTS",
    0x4902: "MEDIA_INFO", 0x4a02: "WAVEFORM_DETAIL", 0x4b02: "UNKNOWN_4B02",
    0x4e02: "CUE_POINTS_EXT", 0x4f02: "ANALYSIS_TAG",
}


def read_packets(path):
    data = open(path, "rb").read()
    packets = []
    pos = 0
    # classic pcap fallback: magic 0xa1b2c3d4 / 0 xa1b23c4d
    if data[:4] in (b"\xa1\xb2\xc3\xd4", b"\x4d\x3c\xb2\xa1",
                    b"\xd4\xc3\xb2\xa1", b"\xa1\xb2\x3c\x4d"):
        be = data[:4] == b"\xa1\xb2\xc3\xd4"
        nano = data[:4] in (b"\xa1\xb2\x3c\x4d", b"\x4d\x3c\xb2\xa1")
        endian = ">" if be else "<"
        linktype = struct.unpack(endian + " I", data[20:24])[0]
        pos = 24
        while pos + 16 <= len(data):
            ts, tus, caplen, origlen = struct.unpack(endian + "IIII", data[pos:pos + 16])
            packets.append((ts + tus / 1e9, data[pos + 16: pos + 16 + caplen]))
            pos += 16 + caplen
        return packets
    while pos + 12 <= len(data):
        btype, blen = struct.unpack_from("<II", data, pos)
        if blen < 12 or pos + blen > len(data):
            break
        if btype == 6:
            caplen = struct.unpack_from("<I", data, pos + 20)[0]
            packets.append((0.0, data[pos + 28: pos + 28 + caplen]))
        pos += blen
    return packets


def flows(packets):
    out = {}
    for _, pkt in packets:
        if len(pkt) < 34 or pkt[12:14] not in (b"\x08\x00", b"\x81\x00"):
            continue
        base = 18 if pkt[12:14] == b"\x81\x00" else 14  # vlan tag
        p = pkt[base:]
        if len(p) < 20 or (p[0] >> 4) != 4:
            continue
        ihl = (p[0] & 0xF) * 4
        if p[9] != 6:  # tcp only
            continue
        s = p[ihl:]
        if len(s) < 20:
            continue
        sport, dport = struct.unpack_from(">HH", s, 0)
        seq, = struct.unpack_from(">I", s, 4)
        doff = (s[12] >> 4) * 4
        body = s[doff:]
        if not body:
            continue
        key = (p[12:16], sport, p[16:20], dport)
        f = out.setdefault(key, {})
        old = f.get(seq)
        if old is None or len(old) < len(body):
            f[seq] = body
    return out


def stream(segs):
    base = min(segs)
    buf = bytearray()
    end = base
    for seq in sorted(segs):
        b = segs[seq]
        s, e = seq, seq + len(b)
        if e <= end:
            continue
        if s > end + (8 << 20):
            break
        if s > end:
            buf.extend(b"\x00" * (s - end))
        if s < end:
            b = b[end - s:]
        buf.extend(b)
        end = max(end, e)
    return bytes(buf)


def parse(buf):
    msgs = []
    pos = 0
    while True:
        i = buf.find(MAGIC, pos)
        if i < 0:
            break
        p = i + 4
        if buf[i - 1:i] == b"\x11":  # tagged variant: the value tag precedes
            pass
        try:
            if buf[p] == 0x11:
                tx = struct.unpack_from(">I", buf, p + 1)[0]; p += 5
            elif buf[p] == 0x10:
                tx = struct.unpack_from(">H", buf, p + 1)[0]; p += 3
            elif buf[p] == 0x0F:
                tx = buf[p + 1]; p += 2
            elif buf[p - 1:i] and buf[i - 1] in (0x11, 0x10):  # magic came pre-tagged
                continue
            else:
                tx = buf[p]; p += 0
            if buf[p] != 0x10:
                continue
            kind = struct.unpack_from(">H", buf, p + 1)[0]; p += 3
            if buf[p] != 0x0F:
                continue
            argc = buf[p + 1]; p += 2
            if buf[p] != 0x14:
                continue
            blen = struct.unpack_from(">I", buf, p + 1)[0]
            if blen > 16:
                continue
            p += 5
            tags = buf[p:p + blen]; p += blen
            args = []
            prev_int = 1
            ok = True
            for k in range(argc):
                if p >= len(buf):
                    ok = False; break
                tag = buf[p]
                if tag in (0x0F, 0x10, 0x11):
                    n = {0x0F: 1, 0x10: 2, 0x11: 4}[tag]
                    v = int.from_bytes(buf[p + 1:p + 1 + n], "big")
                    args.append(("num", v)); prev_int = v; p += 1 + n
                elif tag == 0x14:
                    ln = struct.unpack_from(">I", buf, p + 1)[0]
                    if ln > 32 << 20:
                        ok = False; break
                    if prev_int == 0:
                        args.append(("omit", None))
                    else:
                        args.append(("blob", bytes(buf[p + 5:p + 5 + ln])))
                        p += 5 + ln
                elif tag == 0x26:
                    n = struct.unpack_from(">I", buf, p + 1)[0]
                    if n * 2 > 65536:
                        # u16 count variant
                        n = struct.unpack_from(">H", buf, p + 1)[0]
                        t = bytes(buf[p + 3:p + 3 + n * 2]).decode("utf-16-be", "replace")
                        args.append(("txt", t)); p += 3 + n * 2
                    else:
                        t = bytes(buf[p + 5:p + 5 + n * 2]).decode("utf-16-be", "replace")
                        args.append(("txt", t)); p += 5 + n * 2
                else:
                    ok = False; break
            if not ok:
                continue
            msgs.append((i, tx, kind, args, p))
            pos = max(pos, p)
        except Exception:
            continue
    return msgs


NULSTRIP = lambda s: s.rstrip("\x00")


def main():
    path = sys.argv[1]
    bufout = sys.stdout if len(sys.argv) < 3 else open(sys.argv[2], "w")
    packets = read_packets(path)
    for key, segs in flows(packets).items():
        b = stream(segs)
        for i, tx, kind, args, end in parse(b):
            name = NAME.get(kind, hex(kind))
            desc = ""
            if kind == 0x4000 and args and args[0][0] == "num":
                req = args[0][1]
                desc = f" reply-to={NAME.get(req, hex(req))} count={args[1][1] if len(args) > 1 and args[1][0] == 'num' else '?'}"
            line = [f"tx={tx & 0xffffffff:08x} k=0x{kind:04x} ({name})"]
            for t, v in args:
                if t == "num":
                    four = (v.to_bytes(4, "big") if v > 1 << 20 else b"")
                    if four and all(0x20 <= c < 0x7f for c in four):
                        line.append(f"n={v:#x} '{four.decode()}'")
                    elif v > 4095:
                        line.append(f"n=0x{v:08x}")
                    else:
                        line.append(f"n={v}")
                elif t == "txt":
                    line.append(f"t='{NULSTRIP(v)}'")
                elif t == "blob":
                    line.append(f"blob[{len(v) if isinstance(v, bytes) else v}]")
                else:
                    line.append("omit")
            print(" ".join(line) + desc, file=bufout)


if __name__ == "__main__":
    main()
