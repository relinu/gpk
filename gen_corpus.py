"""Erzeugt einen deterministischen Testkorpus typischer Spiel-Assets."""
import json
import os
import struct
import sys

import numpy as np

OUT = sys.argv[1] if len(sys.argv) > 1 else "corpus"
os.makedirs(OUT, exist_ok=True)
rng = np.random.default_rng(1234)


def fractal_noise(h, w, octaves=6, persistence=0.5, base=8):
    """Mehroktaviges Value-Noise (bilinear interpoliert), Werte ~[0,1]."""
    out = np.zeros((h, w))
    amp, total = 1.0, 0.0
    for o in range(octaves):
        cells = base * 2**o
        grid = rng.random((cells + 1, cells + 1))
        ys = np.linspace(0, cells, h, endpoint=False)
        xs = np.linspace(0, cells, w, endpoint=False)
        y0, x0 = ys.astype(int), xs.astype(int)
        fy, fx = (ys - y0)[:, None], (xs - x0)[None, :]
        fy, fx = fy * fy * (3 - 2 * fy), fx * fx * (3 - 2 * fx)
        a = grid[y0][:, x0]; b = grid[y0][:, x0 + 1]
        c = grid[y0 + 1][:, x0]; d = grid[y0 + 1][:, x0 + 1]
        out += amp * ((a * (1 - fx) + b * fx) * (1 - fy) + (c * (1 - fx) + d * fx) * fy)
        total += amp
        amp *= persistence
    return out / total


def save(name, data):
    path = os.path.join(OUT, name)
    with open(path, "wb") as f:
        f.write(data if isinstance(data, (bytes, bytearray)) else data.tobytes())
    print(f"{name:24s} {os.path.getsize(path):>10,d} Bytes")


# 1) Albedo-Textur RGBA8 1024x1024: Steinmauer mit Fugen, Farbrauschen, Alpha meist 255
H = W = 1024
n = fractal_noise(H, W, octaves=7)
yy, xx = np.mgrid[0:H, 0:W]
row = yy // 64
brick_x = (xx + (row % 2) * 64) % 128
mortar = (yy % 64 < 4) | (brick_x < 4)
base_col = np.array([150, 90, 70]) / 255.0
tint = fractal_noise(H // 128 + 1, W // 128 + 1, octaves=1, base=2)[row.clip(0, H // 128), (xx // 128).clip(0, W // 128)]
col = base_col[None, None, :] * (0.6 + 0.6 * n[..., None]) * (0.85 + 0.3 * tint[..., None])
col[mortar] = np.array([0.55, 0.55, 0.52]) * (0.8 + 0.4 * n[mortar, None])
col += rng.normal(0, 0.012, col.shape)
rgb = (col.clip(0, 1) * 255).astype(np.uint8)
alpha = np.full((H, W, 1), 255, np.uint8)
alpha[fractal_noise(H, W, octaves=3) > 0.72] = 0           # ein paar Löcher (Cutout)
save("texture_rgba8.raw", np.concatenate([rgb, alpha], axis=2))

# 2) Heightmap uint16 1024x1024 (Terrain)
hm = fractal_noise(1024, 1024, octaves=8, persistence=0.55, base=4)
hm16 = (hm * 65535).clip(0, 65535).astype("<u2")
save("heightmap_u16.raw", hm16)

# 3) Normal Map RGB8 512x512 aus Heightmap
h2 = hm[::2, ::2] * 40
gy, gx = np.gradient(h2)
nrm = np.stack([-gx, -gy, np.ones_like(gx)], axis=-1)
nrm /= np.linalg.norm(nrm, axis=-1, keepdims=True)
save("normalmap_rgb8.raw", ((nrm * 0.5 + 0.5) * 255).round().astype(np.uint8))

# 4) Mesh: Terrain-Gitter 320x320, Vertex = pos(3f) normal(3f) uv(2f) = 32 Byte
G = 320
hs = hm[:: 1024 // G, :: 1024 // G][:G, :G] * 120.0
gz, gx2 = np.mgrid[0:G, 0:G].astype(np.float32)
pos = np.stack([gx2 * 2.0, hs.astype(np.float32), gz * 2.0], -1)
dy, dx = np.gradient(hs)
nn = np.stack([-dx, np.full_like(dx, 2.0), -dy], -1)
nn /= np.linalg.norm(nn, axis=-1, keepdims=True)
uv = np.stack([gx2 / (G - 1), gz / (G - 1)], -1)
verts = np.concatenate([pos, nn.astype(np.float32), uv], -1).astype("<f4")
save("mesh_vertices.bin", verts)
# Wie nach einer Vertex-Cache-Optimierung: 8x8-Quad-Kacheln in zufälliger Reihenfolge,
# Dreiecke innerhalb einer Kachel teilweise umsortiert und Windungen rotiert.
q = np.arange(G * G, dtype=np.uint32).reshape(G, G)[:-1, :-1]
tris = []
tiles = [(ty, tx) for ty in range(0, G - 1, 8) for tx in range(0, G - 1, 8)]
for ti in rng.permutation(len(tiles)):
    ty, tx = tiles[ti]
    qq = q[ty:ty + 8, tx:tx + 8].ravel()
    t = np.stack([np.stack([qq, qq + G, qq + 1], -1), np.stack([qq + 1, qq + G, qq + G + 1], -1)], 1).reshape(-1, 3)
    perm = np.arange(len(t)); sw = rng.random(len(t)) < 0.3
    perm[sw] = rng.permutation(perm[sw])
    t = t[perm]
    rot = rng.integers(0, 3, len(t))
    t = np.stack([np.roll(r, k) for r, k in zip(t, rot)])
    tris.append(t)
idx = np.concatenate(tris).astype("<u4")
save("mesh_indices_u32.bin", idx)

# 5) Audio PCM 16-bit Stereo 44.1 kHz, 10 s: Akkorde mit Obertönen, Hüllkurve, leichtes Rauschen
sr, dur = 44100, 10.0
t = np.arange(int(sr * dur)) / sr
sig = np.zeros_like(t)
notes = [220.0, 261.63, 329.63, 392.0, 293.66, 349.23, 440.0, 196.0]
for i, f0 in enumerate(notes * 2):
    start = i * 0.6
    env = np.clip((t - start) * 40, 0, 1) * np.exp(-np.clip(t - start, 0, None) * 2.5) * (t >= start)
    for h_, a_ in [(1, 1.0), (2, 0.5), (3, 0.25), (4, 0.12)]:
        sig += 0.18 * a_ * env * np.sin(2 * np.pi * f0 * h_ * t)
sig += rng.normal(0, 0.004, sig.shape)
left = sig
right = np.roll(sig, 300) * 0.9
pcm = (np.stack([left, right], -1).clip(-1, 1) * 32767).astype("<i2")
save("audio_pcm16_stereo.raw", pcm)

# 6) Level-Tilemap 1024x1024 uint8: Räume, Gänge, Wände, Deko
tm = np.full((1024, 1024), 1, np.uint8)            # 1 = Fels
for _ in range(900):
    rh, rw = rng.integers(6, 30, 2)
    ry, rx = rng.integers(0, 1024 - rh), rng.integers(0, 1024 - rw)
    tm[ry:ry + rh, rx:rx + rw] = 2                  # Wand
    tm[ry + 1:ry + rh - 1, rx + 1:rx + rw - 1] = rng.choice([10, 11, 12])   # Bodentyp
for _ in range(1500):                               # Gänge
    y, x = rng.integers(0, 1024, 2)
    L = rng.integers(10, 80)
    if rng.random() < 0.5: tm[y, x:x + L] = 20
    else: tm[y:y + L, x] = 20
deco = rng.random(tm.shape) < 0.01
tm[deco & (tm >= 10)] = rng.integers(30, 60, deco.sum())[: (deco & (tm >= 10)).sum()]
save("level_tilemap_u8.bin", tm)

# 7) Lokalisierung / Dialoge als JSON (Text)
words = ("der die das und ist nicht ein eine Schwert Schild Drache Burg König Königin Held Quest "
         "Gold Trank Heilung Magie Feuer Eis Wald Höhle Dorf Händler kaufen verkaufen gehen kommen "
         "sehen finden kämpfen Gefahr Schatz Karte Norden Süden Osten Westen schnell langsam alt neu "
         "groß klein Tür Schlüssel Rätsel Zauber Ritter Bogen Pfeil Rüstung Level Erfahrung").split()
entries = {}
for i in range(9000):
    k = rng.integers(5, 18)
    txt = " ".join(rng.choice(words, k)).capitalize() + rng.choice([".", "!", "?", "..."])
    entries[f"dlg_{['npc','quest','item','ui','tutorial'][i % 5]}_{i:05d}"] = {
        "speaker": str(rng.choice(["Aldric", "Mira", "Händler", "Wache", "Erzähler"])),
        "text": txt, "voice": f"vo/dlg_{i:05d}.ogg", "flags": int(rng.integers(0, 4)),
    }
save("dialog_de.json", json.dumps(entries, ensure_ascii=False, indent=1).encode("utf-8"))

# 8) Spielstand: Array von Entity-Structs (64 Byte)
recs = bytearray()
for i in range(16000):
    etype = int(rng.choice([1, 1, 1, 2, 2, 3, 7]))
    p = rng.normal(0, 200, 3)
    yaw = rng.random() * 6.283
    recs += struct.pack("<IHH3f4ff8H", 100000 + i, etype, int(rng.integers(0, 4)) << 4,
                        *p, 0.0, float(np.sin(yaw / 2)), 0.0, float(np.cos(yaw / 2)),
                        float(rng.choice([100.0, 100.0, 75.5, 50.0])),
                        *[int(v) for v in (rng.integers(0, 40, 8) * (rng.random(8) < 0.3))])
save("savegame_entities.bin", bytes(recs))

# 9) Bereits komprimierte Daten (z. B. Ogg/JPEG im Paket) – sollte roh gespeichert werden
save("precompressed_blob.bin", rng.integers(0, 256, 1 << 20, dtype=np.uint8))
