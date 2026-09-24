"""Generates res/app.ico (rounded coral square with a white search ring). Pure stdlib."""
import math, struct, zlib, pathlib

def render(size):
    px = bytearray(size * size * 4)
    ss = 4  # supersampling
    r_corner = size * 0.24
    for y in range(size):
        for x in range(size):
            acc = [0.0, 0.0, 0.0, 0.0]
            for sy in range(ss):
                for sx in range(ss):
                    fx = x + (sx + 0.5) / ss
                    fy = y + (sy + 0.5) / ss
                    # rounded-rect mask
                    dx = max(r_corner - fx, 0, fx - (size - r_corner))
                    dy = max(r_corner - fy, 0, fy - (size - r_corner))
                    if dx * dx + dy * dy > r_corner * r_corner:
                        continue
                    t = (fx + fy) / (2 * size)
                    r = 255
                    g = int(122 - 45 * t)
                    b = int(122 - 13 * t)
                    col = (r, g, b)
                    # search ring + handle
                    cx, cy, rad = size * 0.44, size * 0.44, size * 0.20
                    w = size * 0.075
                    d = math.hypot(fx - cx, fy - cy)
                    white = abs(d - rad) < w / 2
                    hx0, hy0 = cx + rad * 0.72, cy + rad * 0.72
                    hx1, hy1 = size * 0.74, size * 0.74
                    vx, vy = hx1 - hx0, hy1 - hy0
                    tt = max(0, min(1, ((fx - hx0) * vx + (fy - hy0) * vy) / (vx * vx + vy * vy)))
                    if math.hypot(fx - (hx0 + vx * tt), fy - (hy0 + vy * tt)) < w * 0.6:
                        white = True
                    if white:
                        col = (255, 255, 255)
                    acc[0] += col[0]; acc[1] += col[1]; acc[2] += col[2]; acc[3] += 255
            n = ss * ss
            a = acc[3] / n
            i = (y * size + x) * 4
            if a > 0:
                px[i] = int(acc[0] / (acc[3] / 255)); px[i+1] = int(acc[1] / (acc[3] / 255)); px[i+2] = int(acc[2] / (acc[3] / 255))
            px[i+3] = int(a)
    return bytes(px)

def png(size, rgba):
    raw = b"".join(b"\0" + rgba[y*size*4:(y+1)*size*4] for y in range(size))
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))

sizes = [16, 20, 24, 32, 48, 64, 256]
images = [png(s, render(s)) for s in sizes]
out = struct.pack("<HHH", 0, 1, len(sizes))
offset = 6 + 16 * len(sizes)
for s, img in zip(sizes, images):
    out += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(img), offset)
    offset += len(img)
out += b"".join(images)
p = pathlib.Path(__file__).resolve().parent.parent / "res" / "app.ico"
p.write_bytes(out)
print("wrote", p, len(out), "bytes")
