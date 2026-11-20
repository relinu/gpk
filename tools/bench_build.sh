#!/bin/bash
# Comparison on a complete build: ./bench_build.sh <build-dir> <work-dir>
set -e
BUILD=$(realpath "$1"); W=$(realpath -m "$2"); mkdir -p "$W"
GPK=$(realpath ./gpk); ZT=$(realpath ./zstdtool)
RAW=$(du -sb --apparent-size "$BUILD" | cut -f1)
OUT="$W/results.csv"; echo "method,bytes,pack_s,unpack_s" > "$OUT"
t() { local s=$(date +%s.%N); "$@" >/dev/null 2>&1; echo "$(date +%s.%N) - $s" | bc; }
row() { echo "$1,$(stat -c %s "$2"),$3,$4" >> "$OUT"; echo "  $1: $(stat -c %s "$2") bytes, pack ${3}s, unpack ${4}s"; }

echo "Build: $RAW bytes"
# tar stream (files sorted by path) as the base for solid compressors
tar -C "$(dirname "$BUILD")" --sort=name -cf "$W/build.tar" "$(basename "$BUILD")"
row "tar (uncompressed)" "$W/build.tar" 0 0

rm -f "$W/b.zip"; P=$(cd "$BUILD/.." && t zip -9 -r -q -y "$W/b.zip" "$(basename "$BUILD")")
rm -rf "$W/u"; mkdir "$W/u"; X=$(t unzip -q "$W/b.zip" -d "$W/u"); row "zip -9 (Deflate per file)" "$W/b.zip" $P $X

P=$(t sh -c "xz -9e -T1 -k -c '$W/build.tar' > '$W/b.tar.xz'"); X=$(t sh -c "xz -d -c '$W/b.tar.xz' > /dev/null")
row "tar + xz -9e (solid, 64 MiB window)" "$W/b.tar.xz" $P $X

for L in 19 22; do
  P=$(t $ZT c $L "$W/build.tar" "$W/b.zst$L"); X=$(t $ZT d "$W/b.zst$L" "$W/b.unz")
  row "tar + zstd -$L --long=27" "$W/b.zst$L" $P $X
done

gpk_run() {   # name, options...
  local name="$1"; shift
  rm -rf "$W/gx"
  P=$(t $GPK c -t 4 "$@" "$BUILD" "$W/g.gpk"); X=$(t $GPK x -t 4 "$W/g.gpk" "$W/gx")
  diff -r --no-dereference "$BUILD" "$W/gx" >/dev/null || { echo "ROUNDTRIP ERROR for $name"; exit 1; }
  row "$name" "$W/g.gpk" $P $X
}
gpk_run "GPK without preprocessing (LZRC core only)" --no-preproc
gpk_run "GPK preprocessing + xz core" --xz
gpk_run "GPK without filters" --no-filter
gpk_run "GPK without sorting" --no-sort
gpk_run "GPK without deduplication" --no-dedup
gpk_run "GPK (complete)"
# Fair comparison with xz/zstd (both 1 thread): GPK also unpacks with 1 thread
rm -rf "$W/gx"; X=$(t $GPK x -t 1 "$W/g.gpk" "$W/gx"); row "GPK (complete, unpacking with 1 thread)" "$W/g.gpk" - $X
echo "done: $OUT"
