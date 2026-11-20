"""Comparison table from bench_build.sh results: size before/after, difference %, times."""
import subprocess, sys
res, build = sys.argv[1], sys.argv[2]
raw = int(subprocess.check_output(["du", "-sb", "--apparent-size", build]).split()[0])
rows = []
for line in open(res).read().splitlines()[1:]:          # the name may contain commas
    name, b, p, x = line.rsplit(",", 3)
    if not name.startswith("tar (uncomp"):
        rows.append({"method": name, "bytes": b, "pack_s": p, "unpack_s": x})

def mb(b): return f"{b/1e6:,.1f} MB"
def sec(s):
    try: return f"{float(s):.1f} s"
    except ValueError: return "–"

print("| Method | Size before | Size after | Difference | Pack | Unpack |")
print("|---|---:|---:|---:|---:|---:|")
for r in rows:
    after = int(r["bytes"])
    diff = (after - raw) / raw * 100
    print(f"| {r['method']} | {mb(raw)} | {mb(after)} | {diff:+.1f} % | {sec(r['pack_s'])} | {sec(r['unpack_s'])} |")
