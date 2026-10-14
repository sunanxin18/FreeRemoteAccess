/* SPDX-License-Identifier: LGPL-2.1-or-later
 * 私有 HEVC16 原版 C / 独立整数模型门；不加入产品库。
 * 同一文件可单独生成未修改 pred.c 的改名初始化器 translation unit。
 * 编译、链接、执行方式及有限覆盖边界见 README-hevc-intra16.md。
 */
#ifdef FRD_HEVC_ORIGINAL_TU
#ifndef FRD_HEVC_ORIGINAL_PRED
#error FRD_HEVC_ORIGINAL_PRED must name the pinned unmodified pred.c
#endif
#define ff_hevc_pred_init frd_hevc_original_init
#include FRD_HEVC_ORIGINAL_PRED
#else

#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "config.h"
#include "libavcodec/hevc/pred.h"
#include "libavutil/cpu.h"

#if defined(FRD_HEVC_SELECTOR_RED) && defined(FRD_HEVC_REQUIRE_NEON)
#error RED markers must never be present in GREEN
#endif
#if defined(FRD_HEVC_SELECTOR_RED) || defined(FRD_HEVC_REQUIRE_NEON)
#if !ARCH_AARCH64 || !HAVE_NEON_EXTERNAL
#error target selector requires configured AArch64 external NEON
#endif
void ff_hevc_pred_planar_16_8_neon(uint8_t *, const uint8_t *, const uint8_t *, ptrdiff_t);
void ff_hevc_pred_angular_16_8_neon(uint8_t *, const uint8_t *, const uint8_t *, ptrdiff_t, int, int);
#endif

#ifdef FRD_HEVC_SELECTOR_RED
/* 仅供原版 selector RED 取地址；任何调用均是失败，不能作为像素 oracle。 */
void ff_hevc_pred_planar_16_8_neon(uint8_t *d, const uint8_t *t,
                                 const uint8_t *l, ptrdiff_t s)
{
    (void)d; (void)t; (void)l; (void)s;
    fputs("FAIL RED marker called planar\n", stderr);
    abort();
}
void ff_hevc_pred_angular_16_8_neon(uint8_t *d, const uint8_t *t,
                                  const uint8_t *l, ptrdiff_t s, int c, int m)
{
    (void)d; (void)t; (void)l; (void)s; (void)c; (void)m;
    fputs("FAIL RED marker called angular\n", stderr);
    abort();
}
#endif

void frd_hevc_original_init(HEVCPredContext *, int);

enum { N = 16, REF_BYTES = 96, DST_BYTES = 4096, PATTERNS = 46 };
static const uint32_t seed = UINT32_C(0x4c5d2026);
static const int angles[33] = {
    32,26,21,17,13,9,5,2,0,-2,-5,-9,-13,-17,-21,-26,-32,
    -26,-21,-17,-13,-9,-5,-2,0,2,5,9,13,17,21,26,32
};
static const int inverse_angles[15] = {
    -4096,-1638,-910,-630,-482,-390,-315,-256,-315,-390,-482,
    -630,-910,-1638,-4096
};
static uint64_t case_count;

/* 显式向下整除，负数不依赖右移或 C 向零截断。参数范围均远小于 INT_MAX。 */
static int floor_div(int value, int divisor)
{
    return value >= 0 ? value / divisor : -1 - (-1 - value) / divisor;
}
static uint8_t clip_byte(int value)
{
    return (uint8_t)(value < 0 ? 0 : value > 255 ? 255 : value);
}
static void model_planar(uint8_t *dst, const uint8_t *top,
                         const uint8_t *left, ptrdiff_t stride)
{
    for (int y = 0; y < N; y++)
        for (int x = 0; x < N; x++) {
            int sum = (15 - x) * left[y] + (x + 1) * top[16] +
                      (15 - y) * top[x] + (y + 1) * left[16] + 16;
            dst[y * stride + x] = (uint8_t)floor_div(sum, 32);
        }
}
static void model_angular(uint8_t *dst, const uint8_t *top,
                          const uint8_t *left, ptrdiff_t stride, int c_idx, int mode)
{
    int angle = angles[mode - 2], last = floor_div(16 * angle, 32);
    int vertical = mode >= 18;
    const uint8_t *primary = vertical ? top : left;
    const uint8_t *other = vertical ? left : top;
    uint8_t storage[64] = {0}, *extended = storage + 16;
    const uint8_t *ref = primary - 1;
    if (angle < 0 && last < -1) {
        /* 模型只复制数学所需值；原版 TU 仍保留原四字节 copy 的全部合法读取。 */
        for (int k = 0; k <= 16; k++) extended[k] = primary[k - 1];
        for (int k = last; k < 0; k++)
            extended[k] = other[-1 + floor_div(k * inverse_angles[mode - 11] + 128, 256)];
        ref = extended;
    }
    for (int y = 0; y < N; y++)
        for (int x = 0; x < N; x++) {
            int step = vertical ? y + 1 : x + 1;
            int delta = step * angle, idx = floor_div(delta, 32);
            int fact = delta - 32 * idx;
            int pos = (vertical ? x : y) + idx + 1;
            int value = ref[pos];
            if (fact)
                value = floor_div((32 - fact) * ref[pos] + fact * ref[pos + 1] + 16, 32);
            dst[y * stride + x] = (uint8_t)value;
        }
    if (!c_idx && mode == 26)
        for (int y = 0; y < N; y++)
            dst[y * stride] = clip_byte(top[0] + floor_div((int)left[y] - left[-1], 2));
    if (!c_idx && mode == 10)
        for (int x = 0; x < N; x++)
            dst[x] = clip_byte(left[0] + floor_div((int)top[x] - top[-1], 2));
}
static uint32_t random_next(uint32_t *state)
{
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    return *state;
}
static void pattern(uint8_t *top, uint8_t *left, int id)
{
    static const uint8_t constants[] = {0,1,127,128,254,255};
    static const int impulses[] = {0,15,16,31,-1};
    uint32_t state = seed ^ (UINT32_C(0x9e3779b9) * (uint32_t)(id + 1));
    for (int k = -1; k <= 31; k++) {
        int j = k + 1, t = 0, l = 0;
        if (id < 6) t = l = constants[id];
        else if (id == 6 || id == 7) {
            t = id == 6 ? j * 7 : 255 - j * 7;
            l = 255 - t;
        } else if (id == 8 || id == 9) {
            t = ((j + id) & 1) ? 255 : 0;
            l = 255 - t;
        } else if (id >= 10 && id < 20) {
            int slot = id - 10;
            t = slot < 5 && k == impulses[slot % 5] ? 255 : 0;
            l = slot >= 5 && k == impulses[slot % 5] ? 255 : 0;
        } else if (id == 20 || id == 21) {
            t = k == 16 ? (id == 20 ? 255 : 0) : 128;
            l = k == 16 ? (id == 20 ? 0 : 255) : 128;
        } else if (id == 22 || id == 23) {
            t = id == 22 ? 0 : 255;
            l = 255 - t;
        } else if (id >= 24 && id <= 27) {
            t = l = id == 24 ? 0 : id == 25 ? 255 : 128;
        } else if (id == 28 || id == 29) {
            t = (j & 1) + (id == 28 ? 15 : 0);
            l = ((j + 1) & 1) + (id == 28 ? 15 : 0);
        } else {
            t = (int)(random_next(&state) & 255);
            l = (int)(random_next(&state) & 255);
        }
        top[k] = (uint8_t)t; left[k] = (uint8_t)l;
    }
    /* 合同要求共同 top-left，即使两边使用不同存储。 */
    left[-1] = top[-1];
    if (id == 14 || id == 19) top[-1] = left[-1] = 255;
    if (id == 22 || id == 23) top[-1] = left[-1] = 128;
    if (id >= 24 && id <= 27) {
        int corner = id == 24 ? 255 : id == 25 ? 0 : id == 26 ? 131 : 129;
        top[-1] = left[-1] = (uint8_t)corner;
    }
}
static size_t destination_start(ptrdiff_t stride, int residue)
{
    size_t low = 32;
    size_t row0 = low + (stride < 0 ? (size_t)(-stride) * 15 : 0);
    return row0 + (size_t)((residue - (int)(row0 & 15) + 16) & 15);
}
static int one_case(const HEVCPredContext *original, const HEVCPredContext *actual,
                    int id, ptrdiff_t stride, int da, int ta, int la, int mode, int c_idx,
                    int shared)
{
    _Alignas(16) uint8_t top_buffer[REF_BYTES], left_buffer[REF_BYTES];
    _Alignas(16) uint8_t expected[DST_BYTES], from_c[DST_BYTES], selected[DST_BYTES];
    uint8_t top_before[REF_BYTES], left_before[REF_BYTES];
    uint8_t *top = top_buffer + 16 + ta, *left = left_buffer + 16 + la;
    size_t start = destination_start(stride, da);
    memset(top_buffer, 0x9b, sizeof(top_buffer));
    memset(left_buffer, 0x6d, sizeof(left_buffer));
    pattern(top, left, id);
    if (shared) left = top;
    memcpy(top_before, top_buffer, sizeof(top_buffer));
    memcpy(left_before, left_buffer, sizeof(left_buffer));
    memset(expected, 0xa5, sizeof(expected));
    memcpy(from_c, expected, sizeof(from_c));
    memcpy(selected, expected, sizeof(selected));
    if (!mode) {
        model_planar(expected + start, top, left, stride);
        original->pred_planar[2](from_c + start, top, left, stride);
        actual->pred_planar[2](selected + start, top, left, stride);
    } else {
        model_angular(expected + start, top, left, stride, c_idx, mode);
        original->pred_angular[2](from_c + start, top, left, stride, c_idx, mode);
        actual->pred_angular[2](selected + start, top, left, stride, c_idx, mode);
    }
    case_count++;
    if (memcmp(expected, from_c, sizeof(expected)) ||
        memcmp(expected, selected, sizeof(expected)) ||
        memcmp(top_before, top_buffer, sizeof(top_buffer)) ||
        memcmp(left_before, left_buffer, sizeof(left_buffer))) {
        fprintf(stderr, "FAIL pixels/buffer/input pattern=%d stride=%td offsets=%d/%d/%d mode=%d c_idx=%d shared=%d\n",
                id, stride, da, ta, la, mode, c_idx, shared);
        return 1;
    }
    return 0;
}
static int variant(const HEVCPredContext *original, const HEVCPredContext *actual,
                   int id, ptrdiff_t stride, int da, int ta, int la, int shared)
{
    if (one_case(original, actual, id, stride, da, ta, la, 0, 0, shared)) return 1;
    for (int mode = 2; mode <= 34; mode++)
        for (int c_idx = 0; c_idx < 3; c_idx++)
            if (one_case(original, actual, id, stride, da, ta, la, mode, c_idx, shared)) return 1;
    return 0;
}
static int context_equal(const HEVCPredContext *a, const HEVCPredContext *b, int skip_target)
{
    if (a->pred_dc != b->pred_dc) return 0;
    for (int i = 0; i < 4; i++) {
        if (a->intra_pred[i] != b->intra_pred[i]) return 0;
        if (skip_target && i == 2) continue;
        if (a->pred_planar[i] != b->pred_planar[i] ||
            a->pred_angular[i] != b->pred_angular[i]) return 0;
    }
    return 1;
}
static int context_present(const HEVCPredContext *a)
{
    if (!a->pred_dc) return 0;
    for (int i = 0; i < 4; i++)
        if (!a->intra_pred[i] || !a->pred_planar[i] || !a->pred_angular[i]) return 0;
    return 1;
}
static int selector(void)
{
    static const int depths[] = {8,9,10,12,0,7,16};
    HEVCPredContext c8 = {0};
    int target_missing = 0;
    av_force_cpu_flags(0);
    ff_hevc_pred_init(&c8, 8);
    for (size_t d = 0; d < sizeof(depths) / sizeof(depths[0]); d++) {
        int depth = depths[d];
        HEVCPredContext off = {0}, on = {0};
        av_force_cpu_flags(0); ff_hevc_pred_init(&off, depth);
        av_force_cpu_flags(AV_CPU_FLAG_NEON); ff_hevc_pred_init(&on, depth);
        if (!context_present(&off) || !context_present(&on) ||
            !context_equal(&off, &on, depth == 8)) {
            fprintf(stderr, "FAIL context nontarget/null depth=%d\n", depth);
            return 1;
        }
        if (depth == 0 || depth == 7 || depth == 16) {
            if (!context_equal(&off, &c8, 0) || !context_equal(&on, &c8, 0)) {
                fprintf(stderr, "FAIL original unknown-depth behavior depth=%d\n", depth);
                return 1;
            }
        }
        if (depth == 8) {
#if defined(FRD_HEVC_SELECTOR_RED) || defined(FRD_HEVC_REQUIRE_NEON)
            int p_missing = on.pred_planar[2] != ff_hevc_pred_planar_16_8_neon;
            int a_missing = on.pred_angular[2] != ff_hevc_pred_angular_16_8_neon;
            if (!context_equal(&off, &on, 0) &&
                (off.pred_planar[2] == on.pred_planar[2] || off.pred_angular[2] == on.pred_angular[2])) {
                fputs("FAIL partial target transition\n", stderr); return 1;
            }
            if (off.pred_planar[2] == ff_hevc_pred_planar_16_8_neon ||
                off.pred_angular[2] == ff_hevc_pred_angular_16_8_neon) {
                fputs("FAIL NEON selected with CPU off\n", stderr); return 1;
            }
#ifdef FRD_HEVC_SELECTOR_RED
            if (!context_equal(&off, &on, 0) || !p_missing || !a_missing) {
                fputs("FAIL RED prerequisite: actual original target is not unchanged C\n", stderr);
                return 1;
            }
#endif
            target_missing = p_missing || a_missing;
#else
            if (!context_equal(&off, &on, 0)) {
                fputs("FAIL original CPU-off/on context changed\n", stderr); return 1;
            }
#endif
        }
    }
    av_force_cpu_flags(-1);
    if (target_missing) {
#ifdef FRD_HEVC_SELECTOR_RED
        fputs("RED selector: actual ff_hevc_pred_init depth=8 CPU=NEON keeps C; planar16/angular16 target symbols absent from both slots\n", stderr);
        return 3;
#else
        fputs("FAIL selector: required native target symbols not selected\n", stderr);
        return 1;
#endif
    }
#ifdef FRD_HEVC_SELECTOR_RED
    fputs("FAIL RED unexpectedly selected marker\n", stderr); return 1;
#else
    puts("PASS selector context depths=8/9/10/12/0/7/16 CPU=off/on"); return 0;
#endif
}

#ifdef FRD_HEVC_GUARDS
#include "h264-luma-dc-platform.h"
/* 每次分别贴近左/右不可读页；合法参考 [-1..31]，包含原版 copy 至 [18]。 */
static int guarded(const HEVCPredContext *original, const HEVCPredContext *actual)
{
    frd_dc_guard tg = {0}, lg = {0}, dg = {0};
    int result = 1;
    if (frd_dc_guard_create(&tg) || frd_dc_guard_create(&lg) || frd_dc_guard_create(&dg)) goto cleanup;
    if (tg.page_size < 33 || lg.page_size < 33 || dg.page_size < 3871) goto cleanup;
    uint8_t *expected = malloc(dg.page_size), *before_t = malloc(tg.page_size), *before_l = malloc(lg.page_size);
    if (!expected || !before_t || !before_l) {
        free(expected); free(before_t); free(before_l); goto cleanup;
    }
    static const ptrdiff_t strides[] = {16,17,31,32,64,257,-17,-31};
    static const int ids[] = {24,29,30};
    for (int side = 0; side < 2; side++)
        for (size_t si = 0; si < sizeof(strides) / sizeof(strides[0]); si++)
            for (size_t pi = 0; pi < sizeof(ids) / sizeof(ids[0]); pi++) {
                ptrdiff_t stride = strides[si];
                size_t extent = (size_t)(stride < 0 ? -stride : stride) * 15 + 16;
                size_t low = side ? dg.page_size - extent : 0;
                size_t row0 = low + (stride < 0 ? (size_t)(-stride) * 15 : 0);
                uint8_t *top = tg.page + (side ? tg.page_size - 32 : 1);
                uint8_t *left = lg.page + (side ? lg.page_size - 32 : 1);
                if (frd_dc_guard_access(&tg, FRD_DC_READ_WRITE) || frd_dc_guard_access(&lg, FRD_DC_READ_WRITE)) goto free_buffers;
                memset(tg.page, 0x9b, tg.page_size); memset(lg.page, 0x6d, lg.page_size);
                pattern(top, left, ids[pi]);
                memcpy(before_t, tg.page, tg.page_size); memcpy(before_l, lg.page, lg.page_size);
                if (frd_dc_guard_access(&tg, FRD_DC_READ_ONLY) || frd_dc_guard_access(&lg, FRD_DC_READ_ONLY)) goto free_buffers;
                for (int mode = 0; mode <= 34; mode++) {
                    if (mode == 1) continue;
                    for (int c = 0; c < (mode ? 3 : 1); c++) {
                        memset(expected, 0xa5, dg.page_size);
                        if (mode) model_angular(expected + row0, top, left, stride, c, mode);
                        else model_planar(expected + row0, top, left, stride);
                        for (int which = 0; which < 2; which++) {
                            const HEVCPredContext *ctx = which ? actual : original;
                            memset(dg.page, 0xa5, dg.page_size);
                            if (mode) ctx->pred_angular[2](dg.page + row0, top, left, stride, c, mode);
                            else ctx->pred_planar[2](dg.page + row0, top, left, stride);
                            if (memcmp(expected, dg.page, dg.page_size) || memcmp(before_t, tg.page, tg.page_size) || memcmp(before_l, lg.page, lg.page_size)) {
                                fprintf(stderr, "FAIL guard bytes side=%d stride=%td pattern=%d mode=%d c_idx=%d which=%d\n", side, stride, ids[pi], mode, c, which);
                                goto free_buffers;
                            }
                        }
                    }
                }
            }
    result = 0;
free_buffers:
    free(expected); free(before_t); free(before_l);
cleanup:
    if (frd_dc_guard_destroy(&dg)) result = 1;
    if (frd_dc_guard_destroy(&lg)) result = 1;
    if (frd_dc_guard_destroy(&tg)) result = 1;
    return result;
}
#endif

static int reference_cases(void)
{
    HEVCPredContext original = {0}, actual = {0};
    av_force_cpu_flags(0);
    frd_hevc_original_init(&original, 8);
#ifdef FRD_HEVC_REQUIRE_NEON
    av_force_cpu_flags(AV_CPU_FLAG_NEON);
#endif
    ff_hevc_pred_init(&actual, 8);
    if (!context_present(&original) || !context_present(&actual)) return 1;
    for (int id = 0; id < PATTERNS; id++)
        if (variant(&original, &actual, id, 32, 0, 0, 0, 0)) return 1;
    static const ptrdiff_t strides[] = {16,17,31,32,64,257,-17,-31};
    static const int ids[] = {24,29,30};
    for (size_t s = 0; s < sizeof(strides) / sizeof(strides[0]); s++)
        for (size_t p = 0; p < sizeof(ids) / sizeof(ids[0]); p++) {
            if (variant(&original, &actual, ids[p], strides[s], 0, 0, 0, 0)) return 1;
            /* 各地址 mod16 独立扫；不是 16^3 的三重全组合。 */
            for (int axis = 0; axis < 3; axis++)
                for (int residue = 1; residue < 16; residue++)
                    if (variant(&original, &actual, ids[p], strides[s], axis == 0 ? residue : 0,
                                axis == 1 ? residue : 0, axis == 2 ? residue : 0, 0)) return 1;
        }
    /* 共同只读边可完全同址；此组只在 stride32、对齐地址覆盖全部图案。 */
    for (int id = 0; id < PATTERNS; id++)
        if (variant(&original, &actual, id, 32, 0, 0, 0, 1)) return 1;
#ifdef FRD_HEVC_GUARDS
    if (guarded(&original, &actual)) return 1;
#endif
    av_force_cpu_flags(-1);
    printf("PASS reference seed=0x%08" PRIx32 " patterns=%d cases=%" PRIu64 " full-buffer/input comparison\n", seed, PATTERNS, case_count);
    return 0;
}
int main(int argc, char **argv)
{
    if (argc != 2 || (strcmp(argv[1], "--reference") && strcmp(argv[1], "--selector"))) {
        fputs("usage: hevc-intra16-reference --reference | --selector\n", stderr); return 2;
    }
    /* RED 程序只能执行取地址 selector，永不调用 marker。 */
#ifdef FRD_HEVC_SELECTOR_RED
    if (strcmp(argv[1], "--selector")) return 2;
    return selector();
#else
    if (!strcmp(argv[1], "--selector")) return selector();
    if (selector()) return 1;
    return reference_cases();
#endif
}
#endif
