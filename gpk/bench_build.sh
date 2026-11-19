#!/bin/bash
# Vergleich auf einem kompletten Build: ./bench_build.sh <build-ordner> <arbeitsordner>
set -e
BUILD=$(realpath "$1"); W=$(realpath -m "$2"); mkdir -p "$W"
GPK=$(realpath ./gpk); ZT=$(realpath ./zstdtool)
RAW=$(du -sb --apparent-size "$BUILD" | cut -f1)
OUT="$W/results.csv"; echo "methode,bytes,pack_s,entpack_s" > "$OUT"
t() { local s=$(date +%s.%N); "$@" >/dev/null 2>&1; echo "$(date +%s.%N) - $s" | bc; }
row() { echo "$1,$(stat -c %s "$2"),$3,$4" >> "$OUT"; echo "  $1: $(stat -c %s "$2") Bytes, packen ${3}s, entpacken ${4}s"; }

echo "Build: $RAW Bytes"
# tar-Stream (Dateien nach Pfad sortiert) als Basis für solide Kompressoren
tar -C "$(dirname "$BUILD")" --sort=name -cf "$W/build.tar" "$(basename "$BUILD")"
row "tar (unkomprimiert)" "$W/build.tar" 0 0

rm -f "$W/b.zip"; P=$(cd "$BUILD/.." && t zip -9 -r -q -y "$W/b.zip" "$(basename "$BUILD")")
rm -rf "$W/u"; mkdir "$W/u"; X=$(t unzip -q "$W/b.zip" -d "$W/u"); row "zip -9 (Deflate je Datei)" "$W/b.zip" $P $X

P=$(t sh -c "xz -9e -T1 -k -c '$W/build.tar' > '$W/b.tar.xz'"); X=$(t sh -c "xz -d -c '$W/b.tar.xz' > /dev/null")
row "tar + xz -9e (solid, 64 MiB Fenster)" "$W/b.tar.xz" $P $X

for L in 19 22; do
  P=$(t $ZT c $L "$W/build.tar" "$W/b.zst$L"); X=$(t $ZT d "$W/b.zst$L" "$W/b.unz")
  row "tar + zstd -$L --long=27" "$W/b.zst$L" $P $X
done

gpk_run() {   # name, optionen...
  local name="$1"; shift
  rm -rf "$W/gx"
  P=$(t $GPK c -t 4 "$@" "$BUILD" "$W/g.gpk"); X=$(t $GPK x -t 4 "$W/g.gpk" "$W/gx")
  diff -r --no-dereference "$BUILD" "$W/gx" >/dev/null || { echo "ROUNDTRIP-FEHLER bei $name"; exit 1; }
  row "$name" "$W/g.gpk" $P $X
}
gpk_run "GPK ohne Vorverarbeitung (nur LZRC-Kern)" --no-preproc
gpk_run "GPK Vorverarbeitung + xz-Kern" --xz
gpk_run "GPK ohne Filter" --no-filter
gpk_run "GPK ohne Sortierung" --no-sort
gpk_run "GPK ohne Deduplizierung" --no-dedup
gpk_run "GPK (komplett)"
# Fairer Vergleich mit xz/zstd (beide 1 Thread): GPK ebenfalls mit 1 Thread entpacken
rm -rf "$W/gx"; X=$(t $GPK x -t 1 "$W/g.gpk" "$W/gx"); row "GPK (komplett, Entpacken 1 Thread)" "$W/g.gpk" - $X
echo "fertig: $OUT"
