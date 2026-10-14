/* SPDX-License-Identifier: LGPL-2.1-or-later
 * Windows ARM64 私有资格 adapter。无 main/constructor 或产品接线。
 */
#if !defined(_WIN32)
#error "此 adapter 仅支持 Windows；没有 POSIX fallback"
#endif
#if defined(FRD_DC_PLATFORM_CONTRACT)
/* 仅独立测试 TU 先声明 mock 类型/调用。GREEN composition 必须拒绝此宏。
 * 不在普通源中声明假 Win32 ABI；该分支从不授予原生资格。
 */
#if !defined(FRD_DC_WINDOWS_CONTRACT_TYPES_READY)
#error "contract mock 类型只能由独立测试 TU 提供"
#endif
#else
#if defined(_M_ARM64EC) || defined(__arm64ec__) || defined(__ARM64EC__)
#error "私有 Windows gate 不支持 ARM64EC"
#endif
#if !defined(__aarch64__) && !defined(_M_ARM64)
#error "私有 Windows gate 需要原生 ARM64 编译目标"
#endif
#include <windows.h>
_Static_assert(sizeof(void *) == 8, "Windows ARM64 需要 64-bit pointer");
#endif
#include <stdint.h>
#include <stdio.h>
#include "h264-luma-dc-platform.h"
#include "h264-luma-dc-clock-convert.h"
#include "h264-luma-dc-windows-clock-record.h"

static int frd_dc_windows_error(const char *operation, const char *kind, DWORD error) {
    fprintf(stderr, "FAIL platform %s category=%s GetLastError=%lu\n",
            operation, kind, (unsigned long)error);
    return -1;
}
static int frd_dc_windows_zero(const frd_dc_guard *g) {
    return g && !g->base && !g->page && !g->page_size && !g->mapping_size;
}
static int frd_dc_windows_valid(const frd_dc_guard *g) {
    return g && g->base && g->page_size >= 482 && g->page_size <= SIZE_MAX / 3 &&
           g->mapping_size == g->page_size * 3 &&
           (uintptr_t)g->base <= UINTPTR_MAX - g->mapping_size &&
           (uintptr_t)g->page == (uintptr_t)g->base + g->page_size;
}

int frd_dc_guard_destroy(frd_dc_guard *g) {
    if (frd_dc_windows_zero(g)) return 0;
    if (!frd_dc_windows_valid(g))
        return frd_dc_windows_error("guard destroy owner", "invalid-input", ERROR_INVALID_PARAMETER);
    if (!VirtualFree(g->base, 0, MEM_RELEASE)) {
        DWORD error = GetLastError();
        return frd_dc_windows_error("guard VirtualFree", "system-call", error);
    }
    *g = (frd_dc_guard){0};
    return 0;
}

/* create 的原错误和额外 cleanup 错误分别可见；cleanup 失败保留 owner。
 * 无效的系统返回地址仍尝试释放唯一 raw base，不对其执行 pointer arithmetic。
 */
static int frd_dc_windows_create_cleanup(frd_dc_guard *g, const char *operation,
                                         const char *kind, DWORD error) {
    (void)frd_dc_windows_error(operation, kind, error);
    if (!VirtualFree(g->base, 0, MEM_RELEASE)) {
        DWORD cleanup_error = GetLastError();
        (void)frd_dc_windows_error("guard create cleanup VirtualFree", "cleanup", cleanup_error);
    } else {
        *g = (frd_dc_guard){0};
    }
    return -1;
}

int frd_dc_guard_create(frd_dc_guard *g) {
    if (!frd_dc_windows_zero(g))
        return frd_dc_windows_error("guard create owner", "invalid-input", ERROR_INVALID_PARAMETER);
    SYSTEM_INFO info;
    GetSystemInfo(&info);
    size_t page_size = (size_t)info.dwPageSize;
    if (page_size < 482 || page_size > SIZE_MAX / 3)
        return frd_dc_windows_error("guard page size", "range", ERROR_ARITHMETIC_OVERFLOW);
    size_t mapping_size = page_size * 3;
    unsigned char *base = VirtualAlloc(NULL, mapping_size, MEM_RESERVE, PAGE_NOACCESS);
    if (!base) {
        DWORD error = GetLastError();
        return frd_dc_windows_error("guard reserve VirtualAlloc", "system-call", error);
    }
    *g = (frd_dc_guard){base, NULL, page_size, mapping_size};
    if ((uintptr_t)base > UINTPTR_MAX - mapping_size)
        return frd_dc_windows_create_cleanup(g, "guard reserve address", "range", ERROR_ARITHMETIC_OVERFLOW);
    g->page = (unsigned char *)((uintptr_t)base + page_size);
    void *committed = VirtualAlloc(g->page, page_size, MEM_COMMIT, PAGE_READWRITE);
    if (!committed) {
        DWORD error = GetLastError();
        return frd_dc_windows_create_cleanup(g, "guard commit VirtualAlloc", "system-call", error);
    }
    if (committed != g->page)
        return frd_dc_windows_create_cleanup(g, "guard commit address", "unexpected-result", ERROR_INVALID_DATA);
    return 0;
}

int frd_dc_guard_access(frd_dc_guard *g, enum frd_dc_access access) {
    if (!frd_dc_windows_valid(g) || (access != FRD_DC_READ_ONLY && access != FRD_DC_READ_WRITE))
        return frd_dc_windows_error("guard access owner/mode", "invalid-input", ERROR_INVALID_PARAMETER);
    DWORD desired = access == FRD_DC_READ_ONLY ? PAGE_READONLY : PAGE_READWRITE;
    DWORD old_protection = 0;
    if (!VirtualProtect(g->page, g->page_size, desired, &old_protection)) {
        DWORD error = GetLastError();
        return frd_dc_windows_error("guard VirtualProtect", "system-call", error);
    }
    /* 重复 RO/RW 是合法操作；owner 没有私有 protection-state 字段。 */
    if (old_protection != PAGE_READONLY && old_protection != PAGE_READWRITE)
        return frd_dc_windows_error("guard old protection", "unexpected-result", ERROR_INVALID_DATA);
    return 0;
}

/* 单线程私有 executable。QPF 尝试一次，失败不重试或回退。 */
static struct {
    int frequency_checked, frequency_valid;
    DWORD frequency_error;
    frd_dc_windows_clock_record record;
} frd_dc_windows_clock_state;

int frd_dc_clock_ns(uint64_t *value) {
    if (!value)
        return frd_dc_windows_error("clock output", "invalid-input", ERROR_INVALID_PARAMETER);
    if (!frd_dc_windows_clock_state.frequency_checked) {
        LARGE_INTEGER frequency;
        frd_dc_windows_clock_state.frequency_checked = 1;
        if (!QueryPerformanceFrequency(&frequency)) {
            frd_dc_windows_clock_state.frequency_error = GetLastError();
        } else if (frequency.QuadPart <= 0) {
            frd_dc_windows_clock_state.frequency_error = ERROR_INVALID_DATA;
        } else {
            frd_dc_windows_clock_state.record.frequency = (uint64_t)frequency.QuadPart;
            frd_dc_windows_clock_state.frequency_valid = 1;
        }
    }
    if (!frd_dc_windows_clock_state.frequency_valid)
        return frd_dc_windows_error("clock QueryPerformanceFrequency", "cached-failure",
                                    frd_dc_windows_clock_state.frequency_error);
    LARGE_INTEGER counter;
    if (!QueryPerformanceCounter(&counter)) {
        DWORD error = GetLastError();
        return frd_dc_windows_error("clock QueryPerformanceCounter", "system-call", error);
    }
    if (counter.QuadPart < 0)
        return frd_dc_windows_error("clock QPC tick", "negative", ERROR_INVALID_DATA);
    frd_dc_windows_clock_record *r = &frd_dc_windows_clock_state.record;
    uint64_t tick = (uint64_t)counter.QuadPart;
    uint64_t origin = r->count ? r->origin_tick : tick;
    if (tick < origin || (r->count && tick < r->last_tick))
        return frd_dc_windows_error("clock QPC tick", "backward", ERROR_INVALID_DATA);
    uint64_t delta = tick - origin, ns;
    if (frd_dc_ticks_to_ns(delta, r->frequency, &ns))
        return frd_dc_windows_error("clock QPC conversion", "overflow", ERROR_ARITHMETIC_OVERFLOW);
    if (r->count >= FRD_DC_WINDOWS_CLOCK_CAPACITY)
        return frd_dc_windows_error("clock QPC record", "capacity", ERROR_NOT_ENOUGH_MEMORY);
    r->version = 1;
    r->origin_tick = origin;
    r->last_tick = tick;
    r->samples[r->count++] = (frd_dc_windows_clock_sample){tick, delta, ns};
    *value = ns;
    return 0;
}

int frd_dc_windows_clock_read(frd_dc_windows_clock_record *out) {
    if (!out)
        return frd_dc_windows_error("clock record output", "invalid-input", ERROR_INVALID_PARAMETER);
    if (!frd_dc_windows_clock_state.record.count)
        return frd_dc_windows_error("clock record", "uninitialized", ERROR_INVALID_FUNCTION);
    *out = frd_dc_windows_clock_state.record;
    return 0;
}
