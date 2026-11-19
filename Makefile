CC      ?= gcc
CFLAGS  ?= -O2 -march=native -Wall -Wextra
LIBDIR   = /usr/lib/x86_64-linux-gnu

all: bench

bench: bench.c glz.c glz.h
	$(CC) $(CFLAGS) -o $@ bench.c glz.c -lz -lbz2 -llzma $(LIBDIR)/libzstd.so.1 $(LIBDIR)/liblz4.so.1 -lm

corpus:
	python3 gen_corpus.py corpus

results.csv: bench corpus
	./bench corpus/* > results.csv

clean:
	rm -f bench results.csv
