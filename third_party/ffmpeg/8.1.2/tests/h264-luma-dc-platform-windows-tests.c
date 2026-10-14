/* SPDX-License-Identifier: LGPL-2.1-or-later
 * 仅 contract 注入类型/调用，不是 Win32 ABI 或真实 Windows 证明。
 */
#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#define _WIN32 1
#define FRD_DC_PLATFORM_CONTRACT 1
#define FRD_DC_WINDOWS_CONTRACT_TYPES_READY 1
typedef uint32_t DWORD;
typedef int BOOL;
typedef struct { int64_t QuadPart; } LARGE_INTEGER;
/* size_t 是合成边界，不冒充真实 SYSTEM_INFO 的 DWORD page 字段。 */
typedef struct { size_t dwPageSize; } SYSTEM_INFO;
#define MEM_RESERVE 0x2000u
#define MEM_COMMIT 0x1000u
#define MEM_RELEASE 0x8000u
#define PAGE_NOACCESS 0x01u
#define PAGE_READONLY 0x02u
#define PAGE_READWRITE 0x04u
#define ERROR_INVALID_PARAMETER 87u
#define ERROR_ARITHMETIC_OVERFLOW 534u
#define ERROR_INVALID_DATA 13u
#define ERROR_NOT_ENOUGH_MEMORY 8u
#define ERROR_INVALID_FUNCTION 1u

static void require(int ok, const char *name) {
    if (!ok) { fprintf(stderr, "FAIL injected contract %s\n", name); exit(1); }
}
static _Alignas(4096) unsigned char arenas[2][12288];
static size_t mock_page = 4096;
static unsigned reserve_calls, commit_calls, protect_calls, free_calls, info_calls;
static unsigned fail_reserve_at, fail_commit_at, fail_protect_at, fail_free_at;
static int commit_mismatch, reserve_address_overflow;
static DWORD last_error, old_protection = PAGE_READWRITE;
static void *current_base;
static char trace[64];
static size_t trace_size;
static unsigned qpf_calls, qpc_calls;
static BOOL qpf_ok = 1, qpc_ok = 1;
static int64_t frequency = 10000000, tick = 100;
static void note(char c) {
    require(trace_size + 1 < sizeof(trace), "trace bounds");
    trace[trace_size++] = c; trace[trace_size] = 0;
}
static void GetSystemInfo(SYSTEM_INFO *s) { ++info_calls; note('I'); s->dwPageSize = mock_page; }
static DWORD GetLastError(void) { return last_error; }
static void *VirtualAlloc(void *address, size_t size, DWORD kind, DWORD prot) {
    if (kind == MEM_RESERVE) {
        ++reserve_calls; note('R');
        require(!address && size == mock_page * 3 && prot == PAGE_NOACCESS, "reserve args");
        if (reserve_calls == fail_reserve_at) { last_error = 101; return NULL; }
        require(reserve_calls <= 2, "reserve bounds");
        current_base = reserve_address_overflow ? (void *)(uintptr_t)(UINTPTR_MAX - 100) : arenas[reserve_calls - 1];
        return current_base;
    }
    ++commit_calls; note('C');
    require(kind == MEM_COMMIT && prot == PAGE_READWRITE && size == mock_page &&
            (uintptr_t)address == (uintptr_t)current_base + mock_page, "middle commit args");
    if (commit_calls == fail_commit_at) { last_error = 102; return NULL; }
    return commit_mismatch ? (void *)((uintptr_t)address + 1) : address;
}
static BOOL VirtualFree(void *address, size_t size, DWORD kind) {
    ++free_calls; note('F');
    require(address == current_base && !size && kind == MEM_RELEASE, "release exact owner");
    if (free_calls == fail_free_at) { last_error = 104; return 0; }
    return 1;
}
static BOOL VirtualProtect(void *address, size_t size, DWORD prot, DWORD *old) {
    ++protect_calls; note('P');
    require((uintptr_t)address == (uintptr_t)current_base + mock_page && size == mock_page &&
            (prot == PAGE_READONLY || prot == PAGE_READWRITE), "protect only middle");
    if (protect_calls == fail_protect_at) { last_error = 103; return 0; }
    *old = old_protection; old_protection = prot; return 1;
}
static BOOL QueryPerformanceFrequency(LARGE_INTEGER *v) {
    ++qpf_calls; v->QuadPart = frequency;
    if (!qpf_ok) last_error = 105;
    return qpf_ok;
}
static BOOL QueryPerformanceCounter(LARGE_INTEGER *v) {
    ++qpc_calls; v->QuadPart = tick;
    if (!qpc_ok) last_error = 106;
    return qpc_ok;
}

/* 注入只存在此独立 TU。GREEN 不编译/链接此文件。 */
#include "h264-luma-dc-platform-windows.c"

static void reset_guard_mock(void) {
    mock_page = 4096; reserve_calls = commit_calls = protect_calls = free_calls = info_calls = 0;
    fail_reserve_at = fail_commit_at = fail_protect_at = fail_free_at = 0;
    commit_mismatch = reserve_address_overflow = 0;
    last_error = 0; old_protection = PAGE_READWRITE; current_base = NULL;
    trace_size = 0; trace[0] = 0;
}
static int zero_owner(const frd_dc_guard *g) {
    return !g->base && !g->page && !g->page_size && !g->mapping_size;
}
static void guard_cases(void) {
    frd_dc_guard g = {0}, first = {0}, second = {0};
    reset_guard_mock();
    require(!frd_dc_guard_destroy(&g) && !free_calls, "zero destroy");
    require(!frd_dc_guard_create(&g), "create success");
    require(g.base == arenas[0] && g.page == arenas[0] + 4096 &&
            g.page_size == 4096 && g.mapping_size == 12288, "created owner");
    require(!frd_dc_guard_access(&g, FRD_DC_READ_ONLY), "RW to RO");
    require(!frd_dc_guard_access(&g, FRD_DC_READ_ONLY), "repeated RO");
    require(!frd_dc_guard_access(&g, FRD_DC_READ_WRITE), "RO to RW");
    require(!frd_dc_guard_access(&g, FRD_DC_READ_WRITE), "repeated RW");
    require(!frd_dc_guard_destroy(&g) && zero_owner(&g), "release zero owner");
    require(!strcmp(trace, "IRCPPPPF"), "success syscall order");
    require(!frd_dc_guard_destroy(&g) && free_calls == 1, "no double free");
    for (unsigned i = 0; i < 3; ++i) {
        reset_guard_mock(); mock_page = i == 0 ? 0 : i == 1 ? 481 : SIZE_MAX / 3 + 1;
        require(frd_dc_guard_create(&g) == -1 && zero_owner(&g) && !reserve_calls, "page bounds");
    }
    reset_guard_mock(); g.page_size = 1;
    require(frd_dc_guard_create(&g) == -1 && !info_calls, "dirty create precheck");
    require(frd_dc_guard_destroy(&g) == -1 && !free_calls, "dirty destroy precheck");
    require(frd_dc_guard_access(&g, FRD_DC_READ_ONLY) == -1 && !protect_calls, "dirty protect precheck");
    g = (frd_dc_guard){0};
    require(frd_dc_guard_create(NULL) == -1 && frd_dc_guard_destroy(NULL) == -1, "null owner");
    reset_guard_mock(); fail_reserve_at = 1;
    require(frd_dc_guard_create(&g) == -1 && zero_owner(&g) && !strcmp(trace, "IR"), "reserve fail");
    reset_guard_mock(); fail_commit_at = 1;
    require(frd_dc_guard_create(&g) == -1 && zero_owner(&g) && !strcmp(trace, "IRCF"), "commit cleanup");
    reset_guard_mock(); fail_commit_at = fail_free_at = 1;
    require(frd_dc_guard_create(&g) == -1 && !zero_owner(&g) && free_calls == 1, "commit cleanup retained");
    require(!frd_dc_guard_destroy(&g) && zero_owner(&g) && free_calls == 2, "explicit retained retry");
    require(!frd_dc_guard_destroy(&g) && free_calls == 2, "retained no double free");
    reset_guard_mock(); commit_mismatch = 1;
    require(frd_dc_guard_create(&g) == -1 && zero_owner(&g) && free_calls == 1, "commit address mismatch");
    reset_guard_mock(); reserve_address_overflow = 1;
    require(frd_dc_guard_create(&g) == -1 && zero_owner(&g) && !commit_calls && free_calls == 1, "address overflow cleanup");
    reset_guard_mock(); require(!frd_dc_guard_create(&g), "protect fixture");
    require(frd_dc_guard_access(&g, (enum frd_dc_access)9) == -1 && !protect_calls, "invalid mode precheck");
    fail_protect_at = 1;
    require(frd_dc_guard_access(&g, FRD_DC_READ_ONLY) == -1, "RO failure");
    fail_protect_at = 0; require(!frd_dc_guard_access(&g, FRD_DC_READ_ONLY), "RO positive");
    fail_protect_at = 3;
    require(frd_dc_guard_access(&g, FRD_DC_READ_WRITE) == -1, "RW restore failure");
    fail_protect_at = 0; old_protection = PAGE_NOACCESS;
    require(frd_dc_guard_access(&g, FRD_DC_READ_WRITE) == -1, "unexpected old protection");
    frd_dc_guard saved = g; g.mapping_size = 1;
    require(frd_dc_guard_destroy(&g) == -1 && !free_calls, "bad dimensions no free");
    g = saved; fail_free_at = 1;
    require(frd_dc_guard_destroy(&g) == -1 && !memcmp(&g, &saved, sizeof(g)), "free retained owner");
    require(!frd_dc_guard_destroy(&g) && zero_owner(&g), "free retry");
    reset_guard_mock(); require(!frd_dc_guard_create(&first), "first buffer");
    fail_reserve_at = 2;
    require(frd_dc_guard_create(&second) == -1 && zero_owner(&second), "second reserve failure");
    require(!frd_dc_guard_destroy(&first) && free_calls == 1, "first cleaned after second fail");
    reset_guard_mock(); require(!frd_dc_guard_create(&first), "two mapping first");
    require(!frd_dc_guard_create(&second) && first.base != second.base, "two distinct reservations");
    require(!frd_dc_guard_destroy(&second), "two mapping second release");
    current_base = first.base; require(!frd_dc_guard_destroy(&first), "two mapping first release");
    reset_guard_mock(); require(!frd_dc_guard_create(&first), "second commit first");
    fail_commit_at = 2;
    require(frd_dc_guard_create(&second) == -1 && zero_owner(&second) && free_calls == 1, "second commit cleanup");
    current_base = first.base;
    require(!frd_dc_guard_destroy(&first) && free_calls == 2, "first release after second commit fail");
}

static void clock_cases(const char *name) {
    uint64_t out = 777;
    frd_dc_windows_clock_record record;
    require(frd_dc_clock_ns(NULL) == -1 && !qpf_calls && !qpc_calls, "null before syscalls");
    require(frd_dc_windows_clock_read(&record) == -1, "uninitialized record");
    if (!strcmp(name, "qpf-fail") || !strcmp(name, "qpf-zero") || !strcmp(name, "qpf-negative")) {
        qpf_ok = strcmp(name, "qpf-fail") != 0;
        frequency = !strcmp(name, "qpf-zero") ? 0 : -1;
        require(frd_dc_clock_ns(&out) == -1 && out == 777, "bad QPF unchanged");
        qpf_ok = 1; frequency = 10000000;
        require(frd_dc_clock_ns(&out) == -1 && out == 777 && qpf_calls == 1 && !qpc_calls, "QPF once sticky failure");
        return;
    }
    if (!strcmp(name, "qpc-fail") || !strcmp(name, "qpc-negative")) {
        qpc_ok = strcmp(name, "qpc-fail") != 0;
        tick = -1;
        require(frd_dc_clock_ns(&out) == -1 && out == 777, "bad first QPC unchanged");
        require(frd_dc_windows_clock_read(&record) == -1, "no failed tick origin");
        qpc_ok = 1; tick = 200;
        require(!frd_dc_clock_ns(&out) && !out && qpf_calls == 1, "first successful origin");
        require(!frd_dc_windows_clock_read(&record) && record.origin_tick == 200 && record.count == 1, "successful origin record");
        qpc_ok = 0; out = 777;
        require(frd_dc_clock_ns(&out) == -1 && out == 777, "later QPC failed");
        qpc_ok = 1; tick = -1;
        require(frd_dc_clock_ns(&out) == -1 && out == 777, "later negative tick");
        require(!frd_dc_windows_clock_read(&record) && record.last_tick == 200 && record.count == 1, "failed tick state unchanged");
        return;
    }
    if (!strcmp(name, "qpc-overflow")) frequency = 1;
    require(!frd_dc_clock_ns(&out) && !out, "origin ns zero");
    if (!strcmp(name, "qpc-backward")) {
        tick = 101; require(!frd_dc_clock_ns(&out) && out == 100, "forward tick");
        tick = 100; out = 777;
        require(frd_dc_clock_ns(&out) == -1 && out == 777, "backward unchanged");
        require(!frd_dc_windows_clock_read(&record) && record.last_tick == 101 && record.count == 2, "backward state retained");
        tick = 102; require(!frd_dc_clock_ns(&out) && out == 200, "forward after failed backward");
    } else if (!strcmp(name, "qpc-same")) {
        require(!frd_dc_clock_ns(&out) && !out, "same tick valid");
        require(!frd_dc_windows_clock_read(&record) && record.count == 2 && record.samples[1].delta == 0, "same tick recorded");
    } else if (!strcmp(name, "qpc-overflow")) {
        tick = INT64_MAX; out = 777;
        require(frd_dc_clock_ns(&out) == -1 && out == 777, "ns overflow unchanged");
        require(!frd_dc_windows_clock_read(&record) && record.count == 1 && record.last_tick == 100, "overflow retained state");
        tick = 101; require(!frd_dc_clock_ns(&out) && out == UINT64_C(1000000000), "recover valid conversion");
    } else if (!strcmp(name, "clock-record-limit")) {
        for (unsigned i = 1; i < FRD_DC_WINDOWS_CLOCK_CAPACITY; ++i) {
            tick = 100 + i; require(!frd_dc_clock_ns(&out) && out == (uint64_t)i * 100, "record fill");
        }
        out = 777; ++tick;
        require(frd_dc_clock_ns(&out) == -1 && out == 777, "record capacity fails");
        require(!frd_dc_windows_clock_read(&record) && record.count == FRD_DC_WINDOWS_CLOCK_CAPACITY &&
                record.last_tick == 100 + FRD_DC_WINDOWS_CLOCK_CAPACITY - 1, "capacity state unchanged");
        record.count = 0;
        require(!frd_dc_windows_clock_read(&record) && record.count == FRD_DC_WINDOWS_CLOCK_CAPACITY, "snapshot cannot mutate state");
    } else require(0, "unknown clock case");
    require(qpf_calls == 1, "QPF one call");
    require(!frd_dc_windows_clock_read(&record) && record.version == 1 &&
            record.frequency == (uint64_t)frequency && record.origin_tick == 100 &&
            record.samples[0].tick == 100 && !record.samples[0].delta && !record.samples[0].ns, "raw record identity");
    require(frd_dc_windows_clock_read(NULL) == -1, "null snapshot");
}

int main(int argc, char **argv) {
    require(argc == 2, "one explicit case argument");
    if (!strcmp(argv[1], "guard")) guard_cases(); else clock_cases(argv[1]);
    printf("PASS injected logic %s (not native Windows evidence)\n", argv[1]);
    return 0;
}
