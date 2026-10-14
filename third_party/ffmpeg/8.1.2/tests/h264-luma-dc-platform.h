/* SPDX-License-Identifier: LGPL-2.1-or-later
 * 私有保护页/计时接口；不加入产品库，不允许复制 mapping 所有权。
 */
#ifndef FRD_H264_LUMA_DC_PLATFORM_H
#define FRD_H264_LUMA_DC_PLATFORM_H
#include <stddef.h>
#include <stdint.h>

typedef struct {
    unsigned char *base, *page;
    size_t page_size, mapping_size;
} frd_dc_guard;
enum frd_dc_access { FRD_DC_READ_ONLY, FRD_DC_READ_WRITE };

#ifdef _WIN32
/* Windows 实现在独立后继任务中提供，不能落入 POSIX 分支。 */
int frd_dc_guard_create(frd_dc_guard *g);
int frd_dc_guard_access(frd_dc_guard *g, enum frd_dc_access access);
int frd_dc_guard_destroy(frd_dc_guard *g);
int frd_dc_clock_ns(uint64_t *value);
#else
#include <errno.h>
#include <stdio.h>
#include <time.h>
#include <sys/mman.h>
#include <unistd.h>

static inline int frd_dc_error(const char *operation, int error) {
    fprintf(stderr, "FAIL platform %s errno=%d\n", operation, error);
    errno = error;
    return -1;
}
static inline int frd_dc_guard_zero(const frd_dc_guard *g) {
    return g && !g->base && !g->page && !g->page_size && !g->mapping_size;
}
static inline int frd_dc_guard_valid(const frd_dc_guard *g) {
    return g && g->base && g->page_size >= 482 &&
           g->page_size <= SIZE_MAX / 3 && g->mapping_size == g->page_size * 3 &&
           (uintptr_t)g->base <= UINTPTR_MAX - g->mapping_size &&
           (uintptr_t)g->page == (uintptr_t)g->base + g->page_size;
}
static inline int frd_dc_guard_destroy(frd_dc_guard *g) {
    if (frd_dc_guard_zero(g)) return 0;
    if (!frd_dc_guard_valid(g)) return frd_dc_error("guard destroy owner", EINVAL);
    if (munmap(g->base, g->mapping_size)) {
        int error = errno;
        return frd_dc_error("guard munmap", error);
    }
    *g = (frd_dc_guard){0};
    return 0;
}
static inline int frd_dc_guard_create(frd_dc_guard *g) {
    if (!frd_dc_guard_zero(g)) return frd_dc_error("guard create owner", EINVAL);
    long size = sysconf(_SC_PAGESIZE);
    if (size <= 0) return frd_dc_error("guard page size", size < 0 && errno ? errno : EINVAL);
    if ((uintmax_t)size > SIZE_MAX / 3 || size < 482)
        return frd_dc_error("guard page size range", EOVERFLOW);
    size_t page_size = (size_t)size, mapping_size = page_size * 3;
    unsigned char *base = mmap(NULL, mapping_size, PROT_NONE, MAP_PRIVATE | MAP_ANON, -1, 0);
    if (base == MAP_FAILED) {
        int error = errno;
        return frd_dc_error("guard mmap", error);
    }
    *g = (frd_dc_guard){base, base + page_size, page_size, mapping_size};
    if (mprotect(g->page, page_size, PROT_READ | PROT_WRITE)) {
        int error = errno;
        /* cleanup 失败保留唯一 owner，调用方仍能报告并尝试释放。 */
        (void)frd_dc_guard_destroy(g);
        return frd_dc_error("guard initial mprotect", error);
    }
    return 0;
}
static inline int frd_dc_guard_access(frd_dc_guard *g, enum frd_dc_access access) {
    if (!frd_dc_guard_valid(g) || (access != FRD_DC_READ_ONLY && access != FRD_DC_READ_WRITE))
        return frd_dc_error("guard access owner/mode", EINVAL);
    int prot = access == FRD_DC_READ_ONLY ? PROT_READ : PROT_READ | PROT_WRITE;
    if (mprotect(g->page, g->page_size, prot)) {
        int error = errno;
        return frd_dc_error("guard mprotect", error);
    }
    return 0;
}
static inline int frd_dc_clock_ns(uint64_t *value) {
    if (!value) return frd_dc_error("clock output", EINVAL);
    struct timespec t;
    if (clock_gettime(CLOCK_MONOTONIC, &t)) {
        int error = errno;
        return frd_dc_error("clock_gettime", error);
    }
    if (t.tv_sec < 0 || t.tv_nsec < 0 || t.tv_nsec >= 1000000000L)
        return frd_dc_error("clock range", EOVERFLOW);
    uintmax_t sec = (uintmax_t)t.tv_sec;
    if (sec > UINT64_MAX / UINT64_C(1000000000))
        return frd_dc_error("clock seconds overflow", EOVERFLOW);
    uint64_t result = (uint64_t)sec * UINT64_C(1000000000);
    if ((uint64_t)t.tv_nsec > UINT64_MAX - result)
        return frd_dc_error("clock nanoseconds overflow", EOVERFLOW);
    *value = result + (uint64_t)t.tv_nsec;
    return 0;
}
#endif
#endif
