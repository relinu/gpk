"""Erzeugt Markdown-Tabellen aus results.csv."""
import csv, math, sys
rows = list(csv.DictReader(open(sys.argv[1] if len(sys.argv) > 1 else "results.csv")))
files = list(dict.fromkeys(r["file"] for r in rows))
codecs = list(dict.fromkeys(r["codec"] for r in rows))
R = {(r["file"], r["codec"]): r for r in rows}

print("### Kompressionsrate je Datei (Original / komprimiert, höher = besser)\n")
print("| Datei | " + " | ".join(codecs) + " |")
print("|---|" + "---:|" * len(codecs))
for f in files:
    vals = [float(R[f, c]["ratio"]) for c in codecs]
    best = max(vals)
    cells = [f"**{v:.2f}**" if v == best else f"{v:.2f}" for v in vals]
    print(f"| {f} | " + " | ".join(cells) + " |")

print("\n### Gesamtergebnis über alle Dateien\n")
print("| Codec | Gesamtgröße (MB) | Gesamtrate | Kompression MB/s | Dekompression MB/s |")
print("|---|---:|---:|---:|---:|")
raw = sum(int(R[f, codecs[0]]["raw"]) for f in files)
print(f"| *Original* | {raw/1e6:.2f} | 1.00 | – | – |")
for c in codecs:
    cmp_ = sum(int(R[f, c]["compressed"]) for f in files)
    ct = sum(int(R[f, c]["raw"]) / float(R[f, c]["comp_MBps"]) for f in files)
    dt = sum(int(R[f, c]["raw"]) / float(R[f, c]["decomp_MBps"]) for f in files)
    print(f"| {c} | {cmp_/1e6:.2f} | {raw/cmp_:.2f} | {raw/ct:.1f} | {raw/dt:.0f} |")

print("\n### Von GLZ -9 gewählte Filter (je 1-MiB-Block)\n")
print("| Datei | Filter |")
print("|---|---|")
for f in files:
    print(f"| {f} | {R[f, 'GLZ -9']['filters']} |")
