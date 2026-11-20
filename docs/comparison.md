| Method | Size before | Size after | Difference | Pack | Unpack |
|---|---:|---:|---:|---:|---:|
| zip -9 (Deflate per file) | 320.6 MB | 180.3 MB | -43.7 % | 30.6 s | 2.1 s |
| tar + xz -9e (solid, 64 MiB window) | 320.6 MB | 142.2 MB | -55.7 % | 236.3 s | 4.9 s |
| tar + zstd -19 --long=27 | 320.6 MB | 155.9 MB | -51.4 % | 141.6 s | 0.7 s |
| tar + zstd -22 --long=27 | 320.6 MB | 155.6 MB | -51.5 % | 179.6 s | 0.8 s |
| GPK v1 without preprocessing (LZRC core only) | 320.6 MB | 141.2 MB | -56.0 % | 113.0 s | 3.1 s |
| GPK v1 preprocessing + xz core | 320.6 MB | 133.1 MB | -58.5 % | 56.3 s | 1.4 s |
| GPK v1 without filters | 320.6 MB | 141.0 MB | -56.0 % | 63.7 s | 1.6 s |
| GPK v1 without sorting | 320.6 MB | 133.6 MB | -58.3 % | 54.9 s | 1.4 s |
| GPK v1 without deduplication | 320.6 MB | 133.5 MB | -58.4 % | 60.1 s | 1.8 s |
| GPK v1 (complete) | 320.6 MB | 133.4 MB | -58.4 % | 57.3 s | 1.5 s |
| GPK v1 (complete, unpacking with 1 thread) | 320.6 MB | 133.4 MB | -58.4 % | – | 4.7 s |
| GPK v2 optimized (complete) | 320.6 MB | 121.7 MB | -62.0 % | 76.0 s | 2.0 s |
| GPK v2 optimized (unpacking with 1 thread) | 320.6 MB | 121.7 MB | -62.0 % | – | 5.2 s |
| GPK v2 preprocessing + xz core | 320.6 MB | 121.4 MB | -62.1 % | 74.4 s | 1.9 s |
