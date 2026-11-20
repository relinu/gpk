"""Creates a realistic test game build (Linux, ~330 MB).

Real data: x86-64 system libraries (as engine/plugins), Python sources (as game scripts),
TrueType fonts. Synthetic: DDS textures (real BC1/BC3 with mipmaps), music (Ogg-like,
already compressed), sound effects (WAV), meshes, levels, localization, shaders, video,
JPEG images (needs Pillow). Plus DLC duplicates and symlinks as in real builds.
"""
import glob
import json
import os
import shutil
import struct
import sys
import zlib

import numpy as np

OUT = sys.argv[1] if len(sys.argv) > 1 else "testbuild/MyGame"
rng = np.random.default_rng(2024)
if os.path.exists(OUT):
    shutil.rmtree(OUT)


def path(*p):
    full = os.path.join(OUT, *p)
    os.makedirs(os.path.dirname(full), exist_ok=True)
    return full


def write(rel, data):
    with open(path(*rel.split("/")), "wb") as f:
        f.write(data)


def noise(h, w, octaves=5, base=4, persistence=0.55):
    out = np.zeros((h, w), np.float32)
    amp, tot = 1.0, 0.0
    for o in range(octaves):
        cells = base * 2**o
        g = rng.random((cells + 1, cells + 1)).astype(np.float32)
        ys = np.linspace(0, cells, h, endpoint=False); xs = np.linspace(0, cells, w, endpoint=False)
        y0 = ys.astype(int); x0 = xs.astype(int)
        fy = (ys - y0)[:, None]; fx = (xs - x0)[None, :]
        fy = fy * fy * (3 - 2 * fy); fx = fx * fx * (3 - 2 * fx)
        a = g[y0][:, x0]; b = g[y0][:, x0 + 1]; c = g[y0 + 1][:, x0]; d = g[y0 + 1][:, x0 + 1]
        out += amp * ((a * (1 - fx) + b * fx) * (1 - fy) + (c * (1 - fx) + d * fx) * fy)
        tot += amp; amp *= persistence
    return out / tot


# ---------------------------------------------------------------- Textures
def make_image(n, kind):
    yy, xx = np.mgrid[0:n, 0:n]
    nz = noise(n, n, octaves=6, base=4)
    pal = rng.random((2, 3)) * 0.8 + 0.1
    if kind == "bricks":
        row = yy // (n // 16); bx = (xx + (row % 2) * (n // 16)) % (n // 8)
        mortar = ((yy % (n // 16)) < n // 256 + 1) | (bx < n // 256 + 1)
        col = pal[0] * (0.6 + 0.6 * nz[..., None]); col[mortar] = pal[1] * 0.7
    elif kind == "panels":
        grid = ((xx % (n // 4)) < 3) | ((yy % (n // 4)) < 3)
        col = pal[0] * (0.75 + 0.3 * nz[..., None]); col[grid] *= 0.4
    elif kind == "wood":
        rings = np.sin((xx * 0.02 + nz * 8.0) * 6.0) * 0.5 + 0.5
        col = pal[0][None, None] * (0.5 + 0.5 * rings[..., None])
    else:  # organic: grass/stone/soil
        n2 = noise(n, n, octaves=4, base=8)
        t = np.clip((nz - 0.5) * 4 + 0.5, 0, 1)[..., None]
        col = pal[0] * t + pal[1] * (1 - t); col *= (0.8 + 0.4 * n2[..., None])
    col = col + rng.normal(0, 0.015, col.shape)
    rgb = (np.clip(col, 0, 1) * 255).astype(np.uint8)
    alpha = (np.clip((noise(n, n, 3, 4) - 0.3) * 3, 0, 1) * 255).astype(np.uint8)
    return rgb, alpha


def rgb565(c):
    c = c.astype(np.int32)
    return ((c[..., 0] >> 3) << 11) | ((c[..., 1] >> 2) << 5) | (c[..., 2] >> 3)


def unpack565(v):
    r = (v >> 11) & 31; g = (v >> 5) & 63; b = v & 31
    return np.stack([(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)], -1).astype(np.int32)


def blocks(img):
    h, w = img.shape[:2]
    c = img.shape[2] if img.ndim == 3 else 1
    return img.reshape(h // 4, 4, w // 4, 4, c).transpose(0, 2, 1, 3, 4).reshape(h // 4, w // 4, 16, c)


def bc1(rgb):
    bl = blocks(rgb).astype(np.int32)                  # (by,bx,16,3)
    mx = bl.max(2); mn = bl.min(2)
    c0 = rgb565(mx); c1 = rgb565(mn)
    swap = c0 < c1
    c0, c1 = np.where(swap, c1, c0), np.where(swap, c0, c1)
    e0, e1 = unpack565(c0), unpack565(c1)
    pal = np.stack([e0, e1, (2 * e0 + e1) // 3, (e0 + 2 * e1) // 3], 2)      # (by,bx,4,3)
    d = ((bl[:, :, :, None, :] - pal[:, :, None, :, :]) ** 2).sum(-1)        # (by,bx,16,4)
    idx = d.argmin(-1).astype(np.uint32)
    idx[c0 == c1] = 0
    bits = (idx << (2 * np.arange(16, dtype=np.uint32))).sum(-1).astype(np.uint32)
    out = np.zeros(c0.shape + (8,), np.uint8)
    out[..., 0:2] = c0.astype("<u2")[..., None].view(np.uint8)
    out[..., 2:4] = c1.astype("<u2")[..., None].view(np.uint8)
    out[..., 4:8] = bits.astype("<u4")[..., None].view(np.uint8)
    return out


def bc4_alpha(a):
    bl = blocks(a[..., None])[..., 0].astype(np.int32)   # (by,bx,16)
    a0 = bl.max(2); a1 = bl.min(2)
    t = np.arange(8)
    pal = np.where(t[None, None] == 0, a0[..., None], np.where(t[None, None] == 1, a1[..., None],
                   ((8 - t) * a0[..., None] + (t - 1) * a1[..., None]) // 7))
    idx = np.abs(bl[..., None] - pal[:, :, None, :]).argmin(-1).astype(np.uint64)
    idx[a0 == a1] = 0
    bits = (idx << (3 * np.arange(16, dtype=np.uint64))).sum(-1).astype(np.uint64)
    out = np.zeros(a0.shape + (8,), np.uint8)
    out[..., 0] = a0; out[..., 1] = a1
    out[..., 2:8] = bits.astype("<u8")[..., None].view(np.uint8)[..., :6]
    return out


def dds(rgb, alpha, fmt):
    data = []
    w = rgb.shape[0]; mips = 0
    while w >= 4:
        if fmt == "DXT1":
            data.append(bc1(rgb).tobytes())
        else:
            data.append(np.concatenate([bc4_alpha(alpha), bc1(rgb)], -1).tobytes())
        mips += 1
        if w == 4: break
        rgb = rgb.reshape(w // 2, 2, w // 2, 2, 3).mean((1, 3)).astype(np.uint8)
        alpha = alpha.reshape(w // 2, 2, w // 2, 2).mean((1, 3)).astype(np.uint8)
        w //= 2
    n = rgb.shape[0] * 2 ** (mips - 1) if mips > 1 else rgb.shape[0]
    hdr = bytearray(128)
    hdr[0:4] = b"DDS "
    struct.pack_into("<7I", hdr, 4, 124, 0x000A1007, n, n, len(data[0]), 0, mips)
    struct.pack_into("<2I4s", hdr, 76, 32, 0x4, fmt.encode())
    struct.pack_into("<I", hdr, 108, 0x401008)
    return bytes(hdr) + b"".join(data)


kinds = ["bricks", "panels", "wood", "organic"]
tex_specs = [(2048, "DXT5")] * 8 + [(1024, "DXT1")] * 18 + [(1024, "DXT5")] * 6 + [(512, "DXT1")] * 14
tex_names = []
for i, (n, fmt) in enumerate(tex_specs):
    kind = kinds[i % 4]
    rgb, alpha = make_image(n, kind)
    name = f"{kind}_{i:02d}_{'albedo' if fmt == 'DXT1' else 'albedo_alpha'}.dds"
    write(f"MyGame_Data/Textures/{name}", dds(rgb, alpha, fmt))
    tex_names.append(name)
print("textures done")

# UI images as PNG (already zlib-compressed)
def png(rgba):
    h, w = rgba.shape[:2]
    raw = b"".join(b"\x00" + rgba[y].tobytes() for y in range(h))
    def chunk(t, d): return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + \
        chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
for i in range(12):
    rgb, alpha = make_image(512, kinds[i % 4])
    write(f"MyGame_Data/UI/icon_atlas_{i:02d}.png", png(np.concatenate([rgb, alpha[..., None]], -1)))

# ---------------------------------------------------------------- Audio
def oggish(nbytes):
    out = bytearray(); seq = 0
    while len(out) < nbytes:
        payload = rng.integers(0, 256, int(rng.integers(3000, 8000)), dtype=np.uint8).tobytes()
        out += b"OggS\x00\x00" + struct.pack("<qII", seq * 1024, 0x1234, seq) + b"\x00\x00\x00\x00\x01" + bytes([255]) + payload
        seq += 1
    return bytes(out[:nbytes])
for i, name in enumerate(["main_theme", "battle_01", "battle_02", "village", "dungeon", "credits"]):
    write(f"MyGame_Data/Audio/Music/{name}.ogg", oggish(int(rng.integers(3, 6) * 1_000_000)))

def wav(samples, sr=44100):
    pcm = (np.clip(samples, -1, 1) * 32767).astype("<i2")
    ch = pcm.shape[1] if pcm.ndim == 2 else 1
    d = pcm.tobytes()
    return b"RIFF" + struct.pack("<I", 36 + len(d)) + b"WAVEfmt " + struct.pack("<IHHIIHH", 16, 1, ch, sr, sr * ch * 2, ch * 2, 16) + b"data" + struct.pack("<I", len(d)) + d
for i in range(70):
    dur = float(rng.uniform(0.3, 2.5)); t = np.arange(int(44100 * dur)) / 44100
    f0 = float(rng.uniform(80, 900)); env = np.exp(-t * rng.uniform(2, 9))
    s = env * (0.5 * np.sin(2 * np.pi * f0 * t * (1 + 0.3 * t)) + 0.25 * np.sin(2 * np.pi * 2.01 * f0 * t))
    s += env * rng.normal(0, float(rng.uniform(0.002, 0.15)), t.shape)
    st = np.stack([s, np.roll(s, int(rng.integers(0, 40))) * 0.9], -1)
    write(f"MyGame_Data/Audio/SFX/sfx_{['hit','step','magic','ui','door'][i % 5]}_{i:02d}.wav", wav(st))
print("audio done")

# ---------------------------------------------------------------- Meshes
def mesh(kind, res):
    u, v = np.meshgrid(np.linspace(0, 1, res, dtype=np.float32), np.linspace(0, 1, res, dtype=np.float32))
    nz = noise(res, res, 4, 4)
    if kind == "terrain":
        p = np.stack([u * 256, nz * 40, v * 256], -1)
    elif kind == "rock":
        th, ph = u * 2 * np.pi, v * np.pi
        r = 1 + 0.35 * (nz - 0.5)
        p = np.stack([r * np.sin(ph) * np.cos(th), r * np.cos(ph), r * np.sin(ph) * np.sin(th)], -1) * 3
    else:
        th = u * 2 * np.pi
        r = 0.5 + 0.05 * nz
        p = np.stack([r * np.cos(th), v * 6, r * np.sin(th)], -1)
    dy, dx = np.gradient(p, axis=(0, 1))
    nrm = np.cross(dx, dy); nrm /= np.linalg.norm(nrm, axis=-1, keepdims=True) + 1e-9
    tan = dx / (np.linalg.norm(dx, axis=-1, keepdims=True) + 1e-9)
    uv = np.stack([u, v], -1) * (4 if kind == "terrain" else 1)
    vtx = np.concatenate([p, nrm, tan, np.ones_like(u)[..., None], uv], -1).astype("<f4")    # 48 Byte
    q = np.arange(res * res, dtype=np.uint32).reshape(res, res)[:-1, :-1].ravel()
    idx = np.stack([q, q + res, q + 1, q + 1, q + res, q + res + 1], -1).ravel()
    idx = idx.astype("<u2" if res * res < 65536 else "<u4")
    hdr = struct.pack("<4sIIII", b"MESH", 1, 48, res * res, idx.size) + b"\x00" * 12
    return hdr + vtx.tobytes() + idx.tobytes()
for i in range(28):
    kind = ["terrain", "rock", "pillar"][i % 3]
    res = int(rng.choice([96, 128, 192, 256]))
    write(f"MyGame_Data/Meshes/{kind}_{i:02d}.mesh", mesh(kind, res))
print("meshes done")

# ---------------------------------------------------------------- Level
for lv in range(12):
    n = 512
    tm = np.full((n, n), 1, np.uint16)
    for _ in range(300):
        h, w = rng.integers(6, 40, 2); y, x = rng.integers(0, n - 40, 2)
        tm[y:y + h, x:x + w] = 2; tm[y + 1:y + h - 1, x + 1:x + w - 1] = rng.choice([10, 11, 12, 13])
    for _ in range(400):
        y, x = rng.integers(0, n, 2); L = int(rng.integers(10, 60))
        if rng.random() < .5: tm[y, x:x + L] = 20
        else: tm[y:y + L, x] = 20
    ents = [{"id": int(1000 * lv + k), "type": str(rng.choice(["enemy_goblin", "enemy_skeleton", "chest", "torch", "npc_villager", "trigger"])),
             "pos": [round(float(v), 3) for v in rng.uniform(0, 512, 3)], "rot": round(float(rng.uniform(0, 360)), 1),
             "props": {"hp": int(rng.choice([50, 100, 150])), "loot_table": f"loot_{int(rng.integers(0, 20)):02d}", "patrol": bool(rng.random() < .3)}}
            for k in range(2500)]
    write(f"MyGame_Data/Levels/level_{lv:02d}.map", b"LVL1" + struct.pack("<II", n, n) + tm.astype("<u2").tobytes())
    write(f"MyGame_Data/Levels/level_{lv:02d}_entities.json", json.dumps({"level": lv, "entities": ents}, indent=2).encode())

# ---------------------------------------------------------------- Localization
# (the word lists are localized sample content in their respective languages)
vocab = {
    "de": "der die das und Schwert Schild Drache Burg König Held Quest Gold Trank Magie Feuer Wald Höhle Dorf Händler kaufen gehen finden kämpfen Schatz Tür Schlüssel Ritter Rüstung".split(),
    "en": "the a and sword shield dragon castle king hero quest gold potion magic fire forest cave village merchant buy go find fight treasure door key knight armor".split(),
    "fr": "le la et épée bouclier dragon château roi héros quête or potion magie feu forêt grotte village marchand acheter aller trouver combattre trésor porte clé chevalier armure".split(),
    "es": "el la y espada escudo dragón castillo rey héroe misión oro poción magia fuego bosque cueva aldea mercader comprar ir encontrar luchar tesoro puerta llave caballero armadura".split(),
    "it": "il la e spada scudo drago castello re eroe missione oro pozione magia fuoco foresta grotta villaggio mercante comprare andare trovare combattere tesoro porta chiave cavaliere armatura".split(),
    "pl": "i miecz tarcza smok zamek król bohater misja złoto mikstura magia ogień las jaskinia wioska kupiec kupić iść znaleźć walczyć skarb drzwi klucz rycerz zbroja".split(),
    "ru": "и меч щит дракон замок король герой задание золото зелье магия огонь лес пещера деревня торговец купить идти найти сражаться сокровище дверь ключ рыцарь доспехи".split(),
}
keys = [(f"{['npc','quest','item','ui','tutorial','achievement'][i % 6]}.{i:05d}", int(rng.integers(4, 16)), rng.integers(0, 10**6)) for i in range(7000)]
for lang, words in vocab.items():
    r2 = np.random.default_rng(7)
    d = {}
    for key, k, seed in keys:
        rr = np.random.default_rng(int(seed))
        d[key] = {"text": " ".join(words[j % len(words)] for j in rr.integers(0, 1000, k)).capitalize() + ".",
                  "voice": f"vo/{lang}/{key}.ogg"}
    write(f"MyGame_Data/Localization/{lang}.json", json.dumps(d, ensure_ascii=False, indent=1).encode("utf-8"))

# ---------------------------------------------------------------- Scripts (real source code)
pydir = sorted(glob.glob("/usr/lib/python3.12"))[0]
total = 0
for f in sorted(glob.glob(pydir + "/**/*.py", recursive=True)):
    if "test" in f or "idlelib" in f: continue
    rel = os.path.relpath(f, pydir)
    shutil.copyfile(f, path("MyGame_Data", "Scripts", rel))
    total += os.path.getsize(f)
    if total > 18_000_000: break

# Shader
for i in range(150):
    lines = ["#version 450", f"// shader variant {i}", "layout(location=0) in vec3 inPos;", "layout(location=1) in vec3 inNormal;",
             "layout(location=2) in vec2 inUV;", "layout(set=0,binding=0) uniform Camera { mat4 view; mat4 proj; } cam;"]
    for d in rng.choice(["FOG", "SHADOWS", "SKINNING", "NORMALMAP", "ALPHATEST", "WIND", "DETAIL"], 3, replace=False):
        lines.append(f"#define USE_{d} 1")
    for k in range(int(rng.integers(30, 120))):
        lines.append(f"    vec3 t{k} = normalize(inNormal * {rng.uniform(0, 2):.4f} + vec3({rng.uniform():.3f}, {rng.uniform():.3f}, {rng.uniform():.3f}));")
    write(f"MyGame_Data/Shaders/variant_{i:03d}.glsl", ("\n".join(lines) + "\n").encode())

# Fonts (real)
for f in sorted(glob.glob("/usr/share/fonts/truetype/**/*.ttf", recursive=True))[:8]:
    shutil.copyfile(f, path("MyGame_Data", "Fonts", os.path.basename(f)))

# Video (already compressed)
write("MyGame_Data/Movies/intro.bk2", b"KB2j" + rng.integers(0, 256, 22_000_000, dtype=np.uint8).tobytes())

# ---------------------------------------------------------------- Program code (real x86-64 binaries)
libdir = "/usr/lib/x86_64-linux-gnu"
shutil.copyfile(f"{libdir}/libclang-18.so.18", path("MyGame.x86_64")); os.chmod(path("MyGame.x86_64"), 0o755)
plugins = ["libstdc++.so.6", "libicuuc.so.74", "libz.so.1", "libbz2.so.1.0", "liblzma.so.5", "libzstd.so.1", "libssl.so.3", "libcrypto.so.3", "libsqlite3.so.0", "libxml2.so.2"]
for lib in plugins:
    src = os.path.realpath(f"{libdir}/{lib}")
    if os.path.exists(src):
        shutil.copyfile(src, path("lib", os.path.basename(src)))
        if os.path.basename(src) != lib:
            os.symlink(os.path.basename(src), path("lib", lib))
for f in sorted(glob.glob("/usr/bin/*"))[:0]:
    pass
# The crash reporter ships its own copies of the same libraries (typical duplicates)
for lib in ["libstdc++.so.6", "libz.so.1", "libssl.so.3", "libcrypto.so.3"]:
    src = os.path.realpath(f"{libdir}/{lib}")
    if os.path.exists(src):
        shutil.copyfile(src, path("tools", "crashreporter", "lib", lib))
shutil.copyfile("/usr/bin/python3.12" if os.path.exists("/usr/bin/python3.12") else sys.executable, path("tools", "crashreporter", "CrashReporter"))

# ---------------------------------------------------------------- DLC with duplicates
for name in tex_names[8:14]:
    shutil.copyfile(path("MyGame_Data", "Textures", name), path("DLC", "WinterPack", "Textures", name))
for i in range(4):
    rgb, alpha = make_image(1024, kinds[i]); rgb = (rgb * 0.7 + 70).astype(np.uint8)
    write(f"DLC/WinterPack/Textures/snow_{kinds[i]}_{i}.dds", dds(rgb, alpha, "DXT1"))
shutil.copyfile(path("MyGame_Data", "Localization", "en.json"), path("DLC", "WinterPack", "Localization", "en.json"))

# Odds and ends
write("config/default.ini", "\n".join(f"[{s}]\n" + "\n".join(f"{k}={int(rng.integers(0, 100))}" for k in ["volume", "fov", "quality", "vsync"]) for s in ["Audio", "Video", "Input"]).encode())
write("README.txt", ("MyGame – installation notes\n" * 40).encode())
write("EULA.txt", (" ".join(vocab["en"]) * 300).encode())
os.makedirs(path("saves", ".keep").rsplit("/", 1)[0], exist_ok=True)

# JPEG images: loading screens, concept art, UI backgrounds (photo-like).
# Created last so that all other files stay unchanged;
# needs Pillow, can be disabled with --no-jpeg (old build content).
if "--no-jpeg" not in sys.argv:
    try:
        from PIL import Image
    except ImportError:
        Image = None
        print("note: Pillow missing, no JPEG files created")
    if Image is not None:
        def landscape(w, h, seed):
            r = np.random.default_rng(seed)
            yy, xx = np.mgrid[0:h, 0:w] / h
            sky = np.stack([0.35 + 0.3 * yy, 0.5 + 0.3 * yy, 0.85 - 0.1 * yy], -1) * r.uniform(0.7, 1.1, 3)
            hz = 0.45 + 0.15 * noise(1, w, 5, 4)[0] * r.uniform(0.5, 1.5)
            ground = (yy > hz[None, :])[..., None]
            tex = noise(h, w, 6, 6)[..., None]
            land = (np.array([0.25, 0.4, 0.15]) + 0.25 * r.random(3)) * (0.6 + 0.6 * tex)
            img = np.where(ground, land, sky)
            for _ in range(int(r.integers(3, 9))):          # trees/rocks/buildings
                cx, cy, rad = r.random(), r.uniform(0.4, 0.9), r.uniform(0.02, 0.12)
                m = ((xx - cx * w / h) ** 2 + ((yy - cy) * 1.6) ** 2) < rad ** 2
                img[m] = img[m] * 0.4 + r.random(3) * 0.6 * (0.7 + 0.3 * tex[m])
            img += r.normal(0, 0.012, img.shape)
            return Image.fromarray((np.clip(img, 0, 1) * 255).astype(np.uint8))
        for i in range(10):
            landscape(1920, 1080, 100 + i).save(path("MyGame_Data", "UI", "Loading", f"loading_{i:02d}.jpg"), quality=90)
        for i in range(6):
            landscape(2048, 1536, 200 + i).save(path("MyGame_Data", "Art", f"concept_{i:02d}.jpg"), quality=92, subsampling=0)
        for i in range(20):
            landscape(1024, 512, 300 + i).save(path("MyGame_Data", "UI", "Backgrounds", f"menu_bg_{i:02d}.jpg"), quality=85)

size = sum(os.path.getsize(os.path.join(d, f)) for d, _, fs in os.walk(OUT) for f in fs if not os.path.islink(os.path.join(d, f)))
print(f"build done: {OUT}, {size/1e6:.1f} MB")
