"""Creates Markdown tables from results.csv."""
import csv, math, sys
rows = list(csv.DictReader(open(sys.argv[1] if len(sys.argv) > 1 else "results.csv")))
files = list(dict.fromkeys(r["file"] for r in rows))
codecs = list(dict.fromkeys(r["codec"] for r in rows))
R = {(r["file"], r["codec"]): r for r in rows}

print("### Compression ratio per file (original / compressed, higher = better)\n")
print("| File | " + " | ".join(codecs) + " |")
print("|---|" + "---:|" * len(codecs))
for f in files:
    vals = [float(R[f, c]["ratio"]) for c in codecs]
    best = max(vals)
    cells = [f"**{v:.2f}**" if v == best else f"{v:.2f}" for v in vals]
    print(f"| {f} | " + " | ".join(cells) + " |")

print("\n### Overall result across all files\n")
print("| Codec | Total size (MB) | Overall ratio | Compression MB/s | Decompression MB/s |")
print("|---|---:|---:|---:|---:|")
raw = sum(int(R[f, codecs[0]]["raw"]) for f in files)
print(f"| *Original* | {raw/1e6:.2f} | 1.00 | – | – |")
for c in codecs:
    cmp_ = sum(int(R[f, c]["compressed"]) for f in files)
    ct = sum(int(R[f, c]["raw"]) / float(R[f, c]["comp_MBps"]) for f in files)
    dt = sum(int(R[f, c]["raw"]) / float(R[f, c]["decomp_MBps"]) for f in files)
    print(f"| {c} | {cmp_/1e6:.2f} | {raw/cmp_:.2f} | {raw/ct:.1f} | {raw/dt:.0f} |")

print("\n### Filters chosen by GLZ -9 (per 1 MiB block)\n")
print("| File | Filter |")
print("|---|---|")
for f in files:
    print(f"| {f} | {R[f, 'GLZ -9']['filters']} |")
