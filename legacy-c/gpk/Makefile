CC      ?= gcc
CFLAGS  ?= -O2 -march=native -Wall -Wextra
LIBDIR   = /usr/lib/x86_64-linux-gnu

all: gpk zstdtool

gpk: gpk.c platform.h lzrc.c lzrc.h tcm.c tcm.h filters.c filters.h pngx.c pngx.h pxc.c pxc.h jpgx.c jpgx.h ../glz.c ../glz.h
	$(CC) $(CFLAGS) -o $@ gpk.c lzrc.c tcm.c filters.c pngx.c pxc.c jpgx.c ../glz.c -lz -llzma -lpthread -lm

# Vergleichswerkzeug: zstd mit Long-Range-Modus (nur Laufzeitbibliothek vorhanden)
zstdtool: zstdtool.c
	$(CC) $(CFLAGS) -o $@ zstdtool.c $(LIBDIR)/libzstd.so.1

# Windows-.exe per Cross-Compiling (MinGW-w64), statisch gelinkt: keine DLLs nötig.
# Ohne liblzma: Option --xz ist in der .exe nicht verfügbar (Standard-Kern LZRC schon).
WIN_CC     ?= x86_64-w64-mingw32-gcc
WIN_CFLAGS ?= -O2 -Wall -Wextra

windows: gpk.exe

gpk.exe: gpk.c platform.h lzrc.c lzrc.h tcm.c tcm.h filters.c filters.h pngx.c pngx.h pxc.c pxc.h jpgx.c jpgx.h ../glz.c ../glz.h
	$(WIN_CC) $(WIN_CFLAGS) -DGPK_NO_XZ -static -o $@ gpk.c lzrc.c tcm.c filters.c pngx.c pxc.c jpgx.c ../glz.c -lz -lpthread -lm

tests/filter_roundtrip.exe: tests/test_filter_roundtrip.c filters.c filters.h
	$(WIN_CC) -O2 -static -o $@ tests/test_filter_roundtrip.c filters.c

clean:
	rm -f gpk zstdtool t_lzrc gpk.exe tests/*.exe

SAN = -O1 -g -fsanitize=address,undefined -fno-sanitize-recover=all

test: tests/test_filter_roundtrip.c tests/test_tcm_fuzz.c tests/test_pxc_fuzz.c tests/test_jpgx_fuzz.c filters.c tcm.c pxc.c jpgx.c
	$(CC) $(SAN) -o tests/filter_roundtrip tests/test_filter_roundtrip.c filters.c && ./tests/filter_roundtrip
	$(CC) $(SAN) -o tests/tcm_fuzz tests/test_tcm_fuzz.c tcm.c -lm && ./tests/tcm_fuzz
	$(CC) $(SAN) -o tests/pxc_fuzz tests/test_pxc_fuzz.c pxc.c && ./tests/pxc_fuzz
	$(CC) $(SAN) -o tests/jpgx_fuzz tests/test_jpgx_fuzz.c jpgx.c && ./tests/jpgx_fuzz
