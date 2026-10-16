/* SPDX-License-Identifier: LGPL-2.1-or-later
 * 私有 HEVC32 原版 C / 独立 floor 模型；不加入产品库。
 * ORIGINAL_TU 只改初始化器名称，原 pred.c/header/template 必须保持不变。
 * 本文件仅有 --reference / --selector 两个固定入口；RED 只能取地址。
 */
#if defined(FRD_HEVC32_SELECTOR_RED) && defined(FRD_HEVC32_REQUIRE_NATIVE)
#error RED marker and required native implementation are mutually exclusive
#endif
#ifdef FRD_HEVC32_ORIGINAL_TU
#if defined(FRD_HEVC32_SELECTOR_RED) || defined(FRD_HEVC32_REQUIRE_NATIVE)
#error original TU must not contain selector markers or native requirements
#endif
#ifndef FRD_HEVC32_ORIGINAL_PRED
#error FRD_HEVC32_ORIGINAL_PRED must name the pinned unmodified pred.c
#endif
#define ff_hevc_pred_init frd_hevc32_original_init
#include FRD_HEVC32_ORIGINAL_PRED
#else

#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "config.h"
#include "config_components.h"
#include "libavcodec/hevc/pred.h"
#include "libavutil/cpu.h"

/* 当前 0002 的非 Windows 私有边界；不由此赋予 COFF/SEH 资格。 */
#if ARCH_AARCH64 && HAVE_NEON_EXTERNAL && CONFIG_HEVC_DECODER && !defined(_WIN32)
#define FRD_HEVC32_HAS_0002_HOOKS 1
void ff_hevc_pred_planar_16_8_neon(uint8_t *, const uint8_t *, const uint8_t *, ptrdiff_t);
void ff_hevc_pred_angular_16_8_neon(uint8_t *, const uint8_t *, const uint8_t *, ptrdiff_t, int, int);
#else
#define FRD_HEVC32_HAS_0002_HOOKS 0
#endif
#if defined(FRD_HEVC32_SELECTOR_RED) || defined(FRD_HEVC32_REQUIRE_NATIVE)
#if !FRD_HEVC32_HAS_0002_HOOKS
#error target selector requires the configured non-Windows AArch64 external NEON boundary
#endif
void ff_hevc_pred_angular_32_8_dispatch_aarch64(uint8_t *, const uint8_t *, const uint8_t *, ptrdiff_t, int, int);
#endif
#ifdef FRD_HEVC32_SELECTOR_RED
/* 仅供真实 selector 取地址；被调用或 signal 均为失败，不能算预期 RED。 */
void ff_hevc_pred_angular_32_8_dispatch_aarch64(uint8_t *dst, const uint8_t *top,
                                             const uint8_t *left, ptrdiff_t stride,
                                             int c_idx, int mode)
{
    (void)dst; (void)top; (void)left; (void)stride; (void)c_idx; (void)mode;
    fputs("FAIL HEVC32 RED marker called\n", stderr);
    abort();
}
#endif

void frd_hevc32_original_init(HEVCPredContext *, int);
enum { N = 32, REF_BYTES = 128, DST_BYTES = 8192, PATTERNS = 46 };
static const uint32_t seed = UINT32_C(0x4c5d2026);
static const ptrdiff_t strides[] = {32,33,63,64,128,257,-33,-63};
static const int angles[33] = {
    32,26,21,17,13,9,5,2,0,-2,-5,-9,-13,-17,-21,-26,-32,
    -26,-21,-17,-13,-9,-5,-2,0,2,5,9,13,17,21,26,32
};
static const int inverse_angles[15] = {
    -4096,-1638,-910,-630,-482,-390,-315,-256,-315,-390,-482,
    -630,-910,-1638,-4096
};
static uint64_t case_count;

/* 所有除数为正；负输入不依赖算术右移或 C 向零除法。 */
static int floor_div(int value, int divisor)
{
    return value >= 0 ? value / divisor : -1 - (-1 - value) / divisor;
}
typedef struct { int low[2], high[2]; } reference_ranges;
static uint8_t model_sample(const uint8_t *top, const uint8_t *left,
                             int side, int index, reference_ranges *ranges)
{
    if (ranges) {
        if (index < ranges->low[side]) ranges->low[side] = index;
        if (index > ranges->high[side]) ranges->high[side] = index;
    }
    return (side ? left : top)[index];
}
static uint8_t model_projected_sample(const uint8_t *top, const uint8_t *left,
                                       int vertical, int pos, int mode,
                                       reference_ranges *ranges)
{
    /* 负坐标按 inverse angle 投影；不模拟候选 scratch copy 或向量读取。 */
    if (pos < 0)
        return model_sample(top, left, vertical,
                            -1 + floor_div(pos * inverse_angles[mode - 11] + 128, 256), ranges);
    return model_sample(top, left, !vertical, pos - 1, ranges);
}
static void model_angular_ranges(uint8_t *dst, const uint8_t *top, const uint8_t *left,
                                 ptrdiff_t stride, int c_idx, int mode,
                                 reference_ranges *ranges)
{
    int angle = angles[mode - 2];
    int vertical = mode >= 18;
    (void)c_idx; /* size32 对 mode10/26 的 luma 同样没有边界修正。 */
    if (ranges) *ranges = (reference_ranges){{64,64}, {-2,-2}};
    for (int y = 0; y < N; y++)
        for (int x = 0; x < N; x++) {
            int delta = (vertical ? y + 1 : x + 1) * angle;
            int idx = floor_div(delta, 32), fact = delta - 32 * idx;
            int pos = (vertical ? x : y) + idx + 1;
            int first = model_projected_sample(top, left, vertical, pos, mode, ranges);
            int value = first;
            if (fact)
                value = floor_div((32 - fact) * first + fact *
                                  model_projected_sample(top, left, vertical, pos + 1, mode, ranges) + 16, 32);
            dst[y * stride + x] = (uint8_t)value;
        }
}
static void model_angular(uint8_t *dst, const uint8_t *top, const uint8_t *left,
                          ptrdiff_t stride, int c_idx, int mode)
{
    model_angular_ranges(dst, top, left, stride, c_idx, mode, NULL);
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
    static const int impulses[] = {0,31,32,63,-1};
    uint32_t state = seed ^ (UINT32_C(0x9e3779b9) * (uint32_t)(id + 1));
    for (int k = -1; k <= 63; k++) {
        int j = k + 1, t = 0, l = 0;
        if (id < 6) t = l = constants[id];
        else if (id == 6 || id == 7) {
            /* 65 个样本保持单调；扩展旧16的互补梯度，不发生 byte 回绕。 */
            t = id == 6 ? j * 3 : 255 - j * 3;
            l = 255 - t;
        } else if (id == 8 || id == 9) {
            t = ((j + id) & 1) ? 255 : 0;
            l = 255 - t;
        } else if (id >= 10 && id < 20) {
            int slot = id - 10;
            t = slot < 5 && k == impulses[slot % 5] ? 255 : 0;
            l = slot >= 5 && k == impulses[slot % 5] ? 255 : 0;
        } else if (id == 20 || id == 21) {
            t = k == 32 ? (id == 20 ? 255 : 0) : 128;
            l = k == 32 ? (id == 20 ? 0 : 255) : 128;
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
    size_t row0 = 32 + (stride < 0 ? (size_t)(-stride) * (N - 1) : 0);
    return row0 + (size_t)((residue - (int)(row0 & 15) + 16) & 15);
}
static int one_case(const HEVCPredContext *original, const HEVCPredContext *actual,
                    int id, ptrdiff_t stride, int da, int ta, int la,
                    int mode, int c_idx, int shared)
{
    _Alignas(16) uint8_t tb[REF_BYTES], lb[REF_BYTES];
    _Alignas(16) uint8_t expected[DST_BYTES], from_c[DST_BYTES], selected[DST_BYTES];
    uint8_t before_t[REF_BYTES], before_l[REF_BYTES];
    uint8_t *top = tb + 16 + ta, *left = lb + 16 + la;
    size_t start = destination_start(stride, da);
    memset(tb, 0x9b, sizeof(tb)); memset(lb, 0x6d, sizeof(lb));
    pattern(top, left, id);
    if (shared) left = top;
    memcpy(before_t, tb, sizeof(tb)); memcpy(before_l, lb, sizeof(lb));
    memset(expected, 0xa5, sizeof(expected));
    memcpy(from_c, expected, sizeof(from_c)); memcpy(selected, expected, sizeof(selected));
    model_angular(expected + start, top, left, stride, c_idx, mode);
    original->pred_angular[3](from_c + start, top, left, stride, c_idx, mode);
    if (memcmp(expected, from_c, sizeof(expected)) ||
        memcmp(before_t, tb, sizeof(tb)) || memcmp(before_l, lb, sizeof(lb))) {
        fprintf(stderr, "FAIL original/model/input id=%d stride=%td offsets=%d/%d/%d mode=%d c=%d shared=%d\n",
                id, stride, da, ta, la, mode, c_idx, shared);
        return 1;
    }
    actual->pred_angular[3](selected + start, top, left, stride, c_idx, mode);
    if (memcmp(expected, selected, sizeof(expected)) ||
        memcmp(before_t, tb, sizeof(tb)) || memcmp(before_l, lb, sizeof(lb))) {
        fprintf(stderr, "FAIL actual/model/input id=%d stride=%td offsets=%d/%d/%d mode=%d c=%d shared=%d\n",
                id, stride, da, ta, la, mode, c_idx, shared);
        return 1;
    }
    case_count++;
    return 0;
}
static int variant(const HEVCPredContext *original, const HEVCPredContext *actual,
                   int id, ptrdiff_t stride, int da, int ta, int la, int shared)
{
    for (int mode = 2; mode <= 34; mode++)
        for (int c = 0; c < 3; c++)
            if (one_case(original, actual, id, stride, da, ta, la, mode, c, shared)) return 1;
    return 0;
}
/* 比较同一实际 TU 的完整 context；只豁免显式指定的精确槽。 */
static int context_equal(const HEVCPredContext *a, const HEVCPredContext *b,
                         int skip_old16, int skip_new32)
{
    if (a->pred_dc != b->pred_dc) return 0;
    for (int i = 0; i < 4; i++) {
        if (a->intra_pred[i] != b->intra_pred[i]) return 0;
        if (!(skip_old16 && i == 2) && a->pred_planar[i] != b->pred_planar[i]) return 0;
        if (!(skip_old16 && i == 2) && !(skip_new32 && i == 3) &&
            a->pred_angular[i] != b->pred_angular[i]) return 0;
    }
    return 1;
}
static int context_present(const HEVCPredContext *ctx)
{
    if (!ctx->pred_dc) return 0;
    for (int i = 0; i < 4; i++)
        if (!ctx->intra_pred[i] || !ctx->pred_planar[i] || !ctx->pred_angular[i]) return 0;
    return 1;
}
static int selector(void)
{
    static const int depths[] = {8,9,10,12,0,7,16};
    HEVCPredContext c8 = {0};
    int result = 1, target_missing = 0;
    av_force_cpu_flags(0); ff_hevc_pred_init(&c8, 8);
    for (size_t d = 0; d < sizeof(depths) / sizeof(depths[0]); d++) {
        int depth = depths[d];
        HEVCPredContext off = {0}, on = {0};
        av_force_cpu_flags(0); ff_hevc_pred_init(&off, depth);
        av_force_cpu_flags(AV_CPU_FLAG_NEON); ff_hevc_pred_init(&on, depth);
        if (!context_present(&off) || !context_present(&on) ||
            !context_equal(&off, &on, depth == 8 && FRD_HEVC32_HAS_0002_HOOKS, depth == 8)) {
            fprintf(stderr, "FAIL context null/nontarget depth=%d\n", depth); goto cleanup;
        }
        if (depth == 0 || depth == 7 || depth == 16) {
            if (!context_equal(&off, &c8, 0, 0) || !context_equal(&on, &c8, 0, 0)) {
                fprintf(stderr, "FAIL unknown-depth C8 preservation depth=%d\n", depth); goto cleanup;
            }
        }
        if (depth != 8) continue;
#if FRD_HEVC32_HAS_0002_HOOKS
        if (on.pred_planar[2] != ff_hevc_pred_planar_16_8_neon ||
            on.pred_angular[2] != ff_hevc_pred_angular_16_8_neon ||
            off.pred_planar[2] == ff_hevc_pred_planar_16_8_neon ||
            off.pred_angular[2] == ff_hevc_pred_angular_16_8_neon) {
            fputs("FAIL current 0002 old16 hook preservation\n", stderr); goto cleanup;
        }
#endif
#if defined(FRD_HEVC32_SELECTOR_RED) || defined(FRD_HEVC32_REQUIRE_NATIVE)
        if (off.pred_angular[3] == ff_hevc_pred_angular_32_8_dispatch_aarch64) {
            fputs("FAIL angular32 selected with CPU off\n", stderr); goto cleanup;
        }
        target_missing = on.pred_angular[3] != ff_hevc_pred_angular_32_8_dispatch_aarch64;
#endif
#ifdef FRD_HEVC32_REQUIRE_NATIVE
        if (target_missing || off.pred_angular[3] == on.pred_angular[3]) {
            fputs("FAIL required angular32 native wrapper not selected\n", stderr); goto cleanup;
        }
#else
        if (off.pred_angular[3] != on.pred_angular[3]) {
            fputs("FAIL baseline angular32 C slot changed\n", stderr); goto cleanup;
        }
#endif
    }
#ifdef FRD_HEVC32_SELECTOR_RED
    if (!target_missing) {
        fputs("FAIL RED unexpectedly selected marker\n", stderr); goto cleanup;
    }
    fputs("RED selector: actual post0002 ff_hevc_pred_init keeps angular32 C; exact wrapper marker absent; old16 hooks preserved; full context depths=8/9/10/12/0/7/16 CPU=off/on\n", stderr);
    result = 3;
#else
    (void)target_missing;
    puts("PASS selector full context depths=8/9/10/12/0/7/16 CPU=off/on; old16 hooks preserved");
    result = 0;
#endif
cleanup:
    av_force_cpu_flags(-1);
    return result;
}
static int reference_cases(void)
{
    HEVCPredContext original = {0}, actual = {0};
    int result = 1;
    uint64_t numeric, layout, shared;
    case_count = 0;
    av_force_cpu_flags(0); frd_hevc32_original_init(&original, 8);
#ifdef FRD_HEVC32_REQUIRE_NATIVE
    av_force_cpu_flags(AV_CPU_FLAG_NEON);
#endif
    ff_hevc_pred_init(&actual, 8);
    if (!context_present(&original) || !context_present(&actual)) goto cleanup;
    for (int id = 0; id < PATTERNS; id++)
        if (variant(&original, &actual, id, 64, 0, 0, 0, 0)) goto cleanup;
    numeric = case_count;
    static const int ids[] = {24,29,30};
    for (size_t si = 0; si < sizeof(strides) / sizeof(strides[0]); si++)
        for (size_t pi = 0; pi < sizeof(ids) / sizeof(ids[0]); pi++) {
            if (variant(&original, &actual, ids[pi], strides[si], 0, 0, 0, 0)) goto cleanup;
            /* 三地址各自扫16种 residue；不是16^3，也非全部图案布局乘积。 */
            for (int axis = 0; axis < 3; axis++)
                for (int residue = 1; residue < 16; residue++)
                    if (variant(&original, &actual, ids[pi], strides[si],
                                axis == 0 ? residue : 0, axis == 1 ? residue : 0,
                                axis == 2 ? residue : 0, 0)) goto cleanup;
        }
    layout = case_count - numeric;
    for (int id = 0; id < PATTERNS; id++)
        if (variant(&original, &actual, id, 64, 0, 0, 0, 1)) goto cleanup;
    shared = case_count - numeric - layout;
    if (numeric != 4554 || layout != 109296 || shared != 4554 || case_count != 118404) {
        fputs("FAIL incomplete case matrix\n", stderr); goto cleanup;
    }
    printf("PASS reference seed=0x%08" PRIx32 " patterns=%d numeric=%" PRIu64
           " layout=%" PRIu64 " shared=%" PRIu64 " cases=%" PRIu64
           " callback_calls=%" PRIu64 " full8192-dst/full128-input comparison\n",
           seed, PATTERNS, numeric, layout, shared, case_count, case_count * 2);
    result = 0;
cleanup:
    av_force_cpu_flags(-1);
    return result;
}
int main(int argc, char **argv)
{
    if (argc != 2 || (strcmp(argv[1], "--reference") && strcmp(argv[1], "--selector"))) {
        fputs("usage: hevc-angular32-reference --reference | --selector\n", stderr); return 2;
    }
#ifdef FRD_HEVC32_SELECTOR_RED
    if (strcmp(argv[1], "--selector")) return 2;
    return selector();
#else
    if (!strcmp(argv[1], "--selector")) return selector();
    if (selector()) return 1;
    return reference_cases();
#endif
}
#endif
