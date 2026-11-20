/* Platform layer: Linux/POSIX and Windows (MinGW-w64). */
#ifndef GPK_PLATFORM_H
#define GPK_PLATFORM_H
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <sys/stat.h>

#define GPK_PATH 4096

#ifdef _WIN32
#include <windows.h>
#include <io.h>
#include <direct.h>
#include <sys/utime.h>
#ifndef S_ISLNK
#define S_ISLNK(m) 0                      /* Windows: symlinks are treated like files when scanning */
#endif
#define lstat stat

static inline double plat_now(void) {
    static LARGE_INTEGER f; LARGE_INTEGER c;
    if (!f.QuadPart) QueryPerformanceFrequency(&f);
    QueryPerformanceCounter(&c);
    return (double)c.QuadPart / (double)f.QuadPart;
}
static inline int plat_mkdir(const char *p) { return _mkdir(p); }
static inline int plat_open_read(const char *p) { return _open(p, _O_RDONLY | _O_BINARY); }
static inline int plat_open_write(const char *p, int create) {
    return _open(p, _O_WRONLY | _O_BINARY | (create ? _O_CREAT | _O_TRUNC : 0), _S_IREAD | _S_IWRITE);
}
static inline int plat_close(int fd) { return _close(fd); }
static inline int plat_set_size(int fd, uint64_t n) { return _chsize_s(fd, (long long)n) ? -1 : 0; }
/* Every worker opens its own handles, so seek + write is safe here. */
static inline int plat_write_at(int fd, const void *buf, uint64_t len, uint64_t off) {
    if (_lseeki64(fd, (long long)off, SEEK_SET) < 0) return -1;
    const uint8_t *p = buf;
    while (len) {
        unsigned k = len > (1u << 30) ? (1u << 30) : (unsigned)len;
        int r = _write(fd, p, k);
        if (r <= 0) return -1;
        p += r; len -= (uint64_t)r;
    }
    return 0;
}
/* Reads exactly len bytes from off; every caller uses its own handle. */
static inline int plat_read_at(int fd, void *buf, uint64_t len, uint64_t off) {
    if (_lseeki64(fd, (long long)off, SEEK_SET) < 0) return -1;
    uint8_t *p = buf;
    while (len) {
        unsigned k = len > (1u << 30) ? (1u << 30) : (unsigned)len;
        int r = _read(fd, p, k);
        if (r <= 0) return -1;
        p += r; len -= (uint64_t)r;
    }
    return 0;
}
static inline void plat_set_mode(const char *p, uint32_t mode) { (void)p; (void)mode; }
static inline void plat_set_mtime(const char *p, int64_t t) { struct __utimbuf64 u = {t, t}; _utime64(p, &u); }
static inline int plat_symlink(const char *target, const char *p) { (void)target; (void)p; return -1; }
static inline int plat_readlink(const char *p, char *buf, size_t n) { (void)p; (void)buf; (void)n; return -1; }
static inline void plat_unlink(const char *p) { _unlink(p); }
static inline void plat_init_console(void) { SetConsoleOutputCP(CP_UTF8); }
/* Windows has no Unix permissions: write portable defaults into the archive
 * (directories 0755, files 0644, read-only files 0444). */
static inline uint32_t plat_portable_mode(unsigned st_mode, int is_dir) {
    if (is_dir) return 0755;
    return (st_mode & _S_IWRITE) ? 0644 : 0444;
}
/* Windows: paths with a backslash or a drive letter could escape the target directory. */
static inline int plat_path_char_ok(char c) { return c != '\\' && c != ':'; }
#else
#include <unistd.h>
#include <time.h>
#include <sys/mman.h>
#ifndef O_BINARY
#define O_BINARY 0
#endif

static inline double plat_now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec * 1e-9; }
static inline int plat_mkdir(const char *p) { return mkdir(p, 0755); }
static inline int plat_open_read(const char *p) { return open(p, O_RDONLY); }
static inline int plat_open_write(const char *p, int create) { return open(p, O_WRONLY | (create ? O_CREAT | O_TRUNC : 0), 0644); }
static inline int plat_close(int fd) { return close(fd); }
static inline int plat_set_size(int fd, uint64_t n) { return ftruncate(fd, (off_t)n); }
static inline int plat_write_at(int fd, const void *buf, uint64_t len, uint64_t off) {
    const uint8_t *p = buf;
    while (len) {
        ssize_t r = pwrite(fd, p, len, (off_t)off);
        if (r <= 0) return -1;
        p += r; len -= (uint64_t)r; off += (uint64_t)r;
    }
    return 0;
}
static inline int plat_read_at(int fd, void *buf, uint64_t len, uint64_t off) {
    uint8_t *p = buf;
    while (len) {
        ssize_t r = pread(fd, p, len, (off_t)off);
        if (r <= 0) return -1;
        p += r; len -= (uint64_t)r; off += (uint64_t)r;
    }
    return 0;
}
static inline void plat_set_mode(const char *p, uint32_t mode) { chmod(p, mode); }
static inline void plat_set_mtime(const char *p, int64_t t) { struct timespec ts[2] = {{t, 0}, {t, 0}}; utimensat(AT_FDCWD, p, ts, 0); }
static inline int plat_symlink(const char *target, const char *p) { return symlink(target, p); }
static inline int plat_readlink(const char *p, char *buf, size_t n) { ssize_t l = readlink(p, buf, n - 1); if (l >= 0) buf[l] = 0; return (int)l; }
static inline void plat_unlink(const char *p) { unlink(p); }
static inline void plat_init_console(void) {}
static inline uint32_t plat_portable_mode(unsigned st_mode, int is_dir) { (void)is_dir; return st_mode & 07777; }
static inline int plat_path_char_ok(char c) { (void)c; return 1; }
#endif

/* Map a file read-only into memory (size > 0). */
static inline const uint8_t *plat_map(const char *path, uint64_t size) {
    if (!size) return NULL;
#ifdef _WIN32
    HANDLE f = CreateFileA(path, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
    if (f == INVALID_HANDLE_VALUE) return NULL;
    HANDLE m = CreateFileMappingA(f, NULL, PAGE_READONLY, 0, 0, NULL);
    CloseHandle(f);
    if (!m) return NULL;
    void *p = MapViewOfFile(m, FILE_MAP_READ, 0, 0, 0);
    CloseHandle(m);                       /* the view keeps the mapping alive */
    return p;
#else
    int fd = open(path, O_RDONLY);
    if (fd < 0) return NULL;
    void *m = mmap(NULL, size, PROT_READ, MAP_PRIVATE, fd, 0);
    close(fd);
    return m == MAP_FAILED ? NULL : m;
#endif
}
static inline void plat_unmap(const uint8_t *p, uint64_t size) {
#ifdef _WIN32
    (void)size; UnmapViewOfFile(p);
#else
    munmap((void *)p, size);
#endif
}
static inline uint64_t plat_file_size(const char *path) {
#ifdef _WIN32
    struct __stat64 st; if (_stat64(path, &st)) return UINT64_MAX; return (uint64_t)st.st_size;
#else
    struct stat st; if (stat(path, &st)) return UINT64_MAX; return (uint64_t)st.st_size;
#endif
}
#endif
