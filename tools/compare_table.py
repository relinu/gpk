"""Vergleichstabelle aus bench_build.sh-Ergebnissen: Größe vorher/nachher, Differenz %, Zeiten."""
import subprocess, sys
res, build = sys.argv[1], sys.argv[2]
raw = int(subprocess.check_output(["du", "-sb", "--apparent-size", build]).split()[0])
rows = []
for line in open(res).read().splitlines()[1:]:          # Name darf Kommas enthalten
    name, b, p, x = line.rsplit(",", 3)
    if not name.startswith("tar (unkomp"):
        rows.append({"methode": name, "bytes": b, "pack_s": p, "entpack_s": x})

def mb(b): return f"{b/1e6:,.1f} MB".replace(",", "X").replace(".", ",").replace("X", ".")
def sec(s):
    try: return f"{float(s):.1f} s".replace(".", ",")
    except ValueError: return "–"

print("| Verfahren | Größe vorher | Größe nachher | Differenz | Packen | Entpacken |")
print("|---|---:|---:|---:|---:|---:|")
for r in rows:
    after = int(r["bytes"])
    diff = (after - raw) / raw * 100
    print(f"| {r['methode']} | {mb(raw)} | {mb(after)} | {diff:+.1f} %".replace(".", ",", 1) + f" | {sec(r['pack_s'])} | {sec(r['entpack_s'])} |")
