CC      ?= gcc
CFLAGS  ?= -O2 -march=native -Wall -Wextra
LIBDIR   = /usr/lib/x86_64-linux-gnu

all: gpk zstdtool

gpk: gpk.c platform.h lzrc.c lzrc.h filters.c filters.h pngx.c pngx.h ../glz.c ../glz.h
	$(CC) $(CFLAGS) -o $@ gpk.c lzrc.c filters.c pngx.c ../glz.c -lz -llzma -lpthread -lm

# Vergleichswerkzeug: zstd mit Long-Range-Modus (nur Laufzeitbibliothek vorhanden)
zstdtool: zstdtool.c
	$(CC) $(CFLAGS) -o $@ zstdtool.c $(LIBDIR)/libzstd.so.1

# Windows-.exe per Cross-Compiling (MinGW-w64), statisch gelinkt: keine DLLs nötig.
# Ohne liblzma: Option --xz ist in der .exe nicht verfügbar (Standard-Kern LZRC schon).
WIN_CC     ?= x86_64-w64-mingw32-gcc
WIN_CFLAGS ?= -O2 -Wall -Wextra

windows: gpk.exe

gpk.exe: gpk.c platform.h lzrc.c lzrc.h filters.c filters.h pngx.c pngx.h ../glz.c ../glz.h
	$(WIN_CC) $(WIN_CFLAGS) -DGPK_NO_XZ -static -o $@ gpk.c lzrc.c filters.c pngx.c ../glz.c -lz -lpthread -lm

tests/filter_roundtrip.exe: tests/test_filter_roundtrip.c filters.c filters.h
	$(WIN_CC) -O2 -static -o $@ tests/test_filter_roundtrip.c filters.c

clean:
	rm -f gpk zstdtool t_lzrc gpk.exe tests/*.exe

test: tests/test_filter_roundtrip.c filters.c
	$(CC) -O1 -g -fsanitize=address,undefined -o tests/filter_roundtrip tests/test_filter_roundtrip.c filters.c && ./tests/filter_roundtrip
