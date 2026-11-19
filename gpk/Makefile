CC      ?= gcc
CFLAGS  ?= -O2 -march=native -Wall -Wextra
LIBDIR   = /usr/lib/x86_64-linux-gnu

all: gpk zstdtool

gpk: gpk.c lzrc.c lzrc.h filters.c filters.h pngx.c pngx.h ../glz.c ../glz.h
	$(CC) $(CFLAGS) -o $@ gpk.c lzrc.c filters.c pngx.c ../glz.c -lz -llzma -lpthread -lm

# Vergleichswerkzeug: zstd mit Long-Range-Modus (nur Laufzeitbibliothek vorhanden)
zstdtool: zstdtool.c
	$(CC) $(CFLAGS) -o $@ zstdtool.c $(LIBDIR)/libzstd.so.1

clean:
	rm -f gpk zstdtool t_lzrc

test: tests/test_filter_roundtrip.c filters.c
	$(CC) -O1 -g -fsanitize=address,undefined -o tests/filter_roundtrip tests/test_filter_roundtrip.c filters.c && ./tests/filter_roundtrip
