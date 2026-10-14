/* SPDX-License-Identifier: LGPL-2.1-or-later
 * 私有纯时钟算术测试；不执行 codec，不是 Windows 原生证明。
 */
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include "h264-luma-dc-clock-convert.h"

static void require(int ok, const char *name) {
    if (!ok) { fprintf(stderr, "FAIL clock math %s\n", name); exit(1); }
}

static void fixed(uint64_t delta, uint64_t freq, int success, uint64_t expected) {
    uint64_t out = UINT64_C(0x13579bdf2468ace0);
    int result = frd_dc_ticks_to_ns(delta, freq, &out);
    require(result == (success ? 0 : -1), "fixed status");
    require(out == (success ? expected : UINT64_C(0x13579bdf2468ace0)), "fixed output");
}

static void emit(uint64_t delta, uint64_t freq) {
    uint64_t out = UINT64_C(0x13579bdf2468ace0);
    int result = frd_dc_ticks_to_ns(delta, freq, &out);
    printf("CLOCK %" PRIu64 " %" PRIu64 " %d %" PRIu64 "\n", delta, freq, result, out);
}

static uint64_t next(uint64_t *seed) {
    *seed ^= *seed << 13; *seed ^= *seed >> 7; *seed ^= *seed << 17;
    return *seed;
}

int main(void) {
    const uint64_t fs[] = {1, 3, UINT64_C(10000000), INT64_MAX};
    fixed(0, 1, 1, 0); fixed(1, 1, 1, UINT64_C(1000000000));
    fixed(1, 3, 1, UINT64_C(333333333));
    fixed(2, 3, 1, UINT64_C(666666666));
    fixed(3, 3, 1, UINT64_C(1000000000));
    fixed(4, 3, 1, UINT64_C(1333333333));
    fixed(1, UINT64_C(10000000), 1, 100);
    fixed((uint64_t)INT64_MAX - 1, INT64_MAX, 1, UINT64_C(999999999));
    fixed((uint64_t)INT64_MAX + 1, INT64_MAX, 1, UINT64_C(1000000000));
    fixed(UINT64_MAX, INT64_MAX, 1, UINT64_C(2000000000));
    fixed(UINT64_C(18446744073), 1, 1, UINT64_C(18446744073000000000));
    fixed(UINT64_C(18446744074), 1, 0, 0);
    fixed(UINT64_C(55340232221), 3, 1, UINT64_C(18446744073666666666));
    fixed(UINT64_C(55340232222), 3, 0, 0);
    fixed(UINT64_C(184467440737095516), UINT64_C(10000000), 1, UINT64_C(18446744073709551600));
    fixed(UINT64_C(184467440737095517), UINT64_C(10000000), 0, 0);
    fixed(UINT64_C(184467440737), 10, 1, UINT64_C(18446744073700000000));
    fixed(UINT64_C(184467440738), 10, 0, 0);
    fixed(UINT64_MAX, UINT64_C(1000000000), 1, UINT64_MAX);
    fixed(UINT64_MAX, UINT64_C(999999999), 0, 0);
    fixed(1, 0, 0, 0); fixed(1, (uint64_t)INT64_MAX + 1, 0, 0);
    require(frd_dc_ticks_to_ns(0, 1, NULL) == -1, "null output");
    for (size_t i = 0; i < sizeof(fs) / sizeof(fs[0]); ++i) {
        const uint64_t f = fs[i];
        const uint64_t ds[] = {0, 1, f - 1, f, f + 1, UINT64_MAX};
        for (size_t j = 0; j < sizeof(ds) / sizeof(ds[0]); ++j) emit(ds[j], f);
    }
    emit(1, 0); emit(1, (uint64_t)INT64_MAX + 1);
    emit(UINT64_C(18446744073), 1); emit(UINT64_C(18446744074), 1);
    emit(UINT64_C(55340232221), 3); emit(UINT64_C(55340232222), 3);
    emit(UINT64_C(184467440737095516), UINT64_C(10000000));
    emit(UINT64_C(184467440737095517), UINT64_C(10000000));
    emit(UINT64_C(184467440737), 10); emit(UINT64_C(184467440738), 10);
    emit(UINT64_MAX, UINT64_C(1000000000)); emit(UINT64_MAX, UINT64_C(999999999));
    uint64_t seed = UINT64_C(0x87a4fc1d529b03e7);
    for (unsigned i = 0; i < 10000; ++i) {
        uint64_t delta = next(&seed), f = next(&seed) & (uint64_t)INT64_MAX;
        if (!f) f = 1;
        emit(delta, f);
    }
    puts("CLOCK-END 10036");
    return 0;
}
