/* SPDX-License-Identifier: LGPL-2.1-or-later
 * 私有 NEON 门；直接复用冻结 reference TU，不复制 oracle。
 */
#if defined(FRD_HEVC_ORIGINAL_TU) || defined(FRD_HEVC_SELECTOR_RED)
#error native wrapper is neither original TU nor marker RED
#endif
#define FRD_HEVC_REQUIRE_NEON 1
#define FRD_HEVC_GUARDS 1
#define main frd_hevc_reference_main
#include "hevc-intra16-reference.c"
#undef main

#if defined(_WIN32)
#error private native ABI/guard closure unavailable for Windows ARM64
#endif
uint64_t frd_hevc_intra16_abi(uint8_t *, const uint8_t *, const uint8_t *,
                             ptrdiff_t, int, int, int);

/* planar 不读取 corner；两端分别贴紧 [0] 和 [16]。不在短页上调用 pattern。 */
static int planar_minimum_guards(void)
{
    frd_dc_guard tg = {0}, lg = {0}, dg = {0};
    uint8_t *expected = NULL, *before_t = NULL, *before_l = NULL;
    HEVCPredContext actual = {0};
    int result = 1;
    uint64_t count = 0;
    static const ptrdiff_t strides[] = {16,17,31,32,64,257,-17,-31};
    av_force_cpu_flags(AV_CPU_FLAG_NEON);
    ff_hevc_pred_init(&actual, 8);
    if (actual.pred_planar[2] != ff_hevc_pred_planar_16_8_neon) goto cleanup;
    if (frd_dc_guard_create(&tg) || frd_dc_guard_create(&lg) || frd_dc_guard_create(&dg)) goto cleanup;
    if (dg.page_size < 3871) goto cleanup;
    expected = malloc(dg.page_size); before_t = malloc(tg.page_size); before_l = malloc(lg.page_size);
    if (!expected || !before_t || !before_l) goto cleanup;
    for (int side = 0; side < 2; side++)
        for (int shared = 0; shared < 2; shared++)
            for (int id = 0; id < PATTERNS; id++)
                for (size_t si = 0; si < sizeof(strides)/sizeof(strides[0]); si++) {
                    uint8_t tb[REF_BYTES], lb[REF_BYTES];
                    pattern(tb + 16, lb + 16, id);
                    uint8_t *top = tg.page + (side ? tg.page_size - 17 : 0);
                    uint8_t *left = lg.page + (side ? lg.page_size - 17 : 0);
                    ptrdiff_t stride = strides[si];
                    size_t extent = (size_t)(stride < 0 ? -stride : stride) * 15 + 16;
                    size_t low = side ? dg.page_size - extent : 0;
                    size_t row0 = low + (stride < 0 ? (size_t)(-stride) * 15 : 0);
                    if (frd_dc_guard_access(&tg, FRD_DC_READ_WRITE) || frd_dc_guard_access(&lg, FRD_DC_READ_WRITE)) goto cleanup;
                    memset(tg.page, 0x9b, tg.page_size); memset(lg.page, 0x6d, lg.page_size);
                    memcpy(top, tb + 16, 17); memcpy(left, lb + 16, 17);
                    memcpy(before_t, tg.page, tg.page_size); memcpy(before_l, lg.page, lg.page_size);
                    if (shared) left = top;
                    if (frd_dc_guard_access(&tg, FRD_DC_READ_ONLY) || frd_dc_guard_access(&lg, FRD_DC_READ_ONLY)) goto cleanup;
                    memset(expected, 0xa5, dg.page_size); memset(dg.page, 0xa5, dg.page_size);
                    model_planar(expected + row0, top, left, stride);
                    actual.pred_planar[2](dg.page + row0, top, left, stride);
                    if (memcmp(expected, dg.page, dg.page_size) || memcmp(before_t, tg.page, tg.page_size) || memcmp(before_l, lg.page, lg.page_size)) {
                        fprintf(stderr, "FAIL planar minimum guard side=%d shared=%d id=%d stride=%td\n", side, shared, id, stride);
                        goto cleanup;
                    }
                    count++;
                }
    printf("PASS planar minimum RO [0..16] guards cases=%" PRIu64 "\n", count);
    result = 0;
cleanup:
    av_force_cpu_flags(-1);
    free(expected); free(before_t); free(before_l);
    if (frd_dc_guard_destroy(&dg)) result = 1;
    if (frd_dc_guard_destroy(&lg)) result = 1;
    if (frd_dc_guard_destroy(&tg)) result = 1;
    return result;
}

/* helper 的 BL 直接指向数值符号，返回前先检查全部 nonvolatile；没有 C restoring bridge。 */
static int abi_cases(void)
{
    _Alignas(16) uint8_t tb[REF_BYTES], lb[REF_BYTES], expected[DST_BYTES], output[DST_BYTES];
    static const ptrdiff_t strides[] = {16,17,31,32,64,257,-17,-31};
    uint8_t before_t[REF_BYTES], before_l[REF_BYTES];
    uint64_t count = 0;
    for (int id = 0; id < PATTERNS; id++)
        for (int shared = 0; shared < 2; shared++)
            for (size_t si = 0; si < sizeof(strides)/sizeof(strides[0]); si++)
                for (int mode = 0; mode <= 34; mode++) {
                    if (mode == 1) continue;
                    for (int c = 0; c < (mode ? 3 : 1); c++) {
                        uint8_t *top = tb + 17, *left = lb + 19;
                        ptrdiff_t stride = strides[si];
                        size_t start = destination_start(stride, 7);
                        memset(tb, 0x9b, sizeof(tb)); memset(lb, 0x6d, sizeof(lb));
                        pattern(top, left, id); if (shared) left = top;
                        memcpy(before_t, tb, sizeof(tb)); memcpy(before_l, lb, sizeof(lb));
                        memset(expected, 0xa5, sizeof(expected)); memset(output, 0xa5, sizeof(output));
                        if (mode) model_angular(expected + start, top, left, stride, c, mode);
                        else model_planar(expected + start, top, left, stride);
                        uint64_t failure = frd_hevc_intra16_abi(output + start, top, left, stride, c, mode, !!mode);
                        if (failure || memcmp(expected, output, sizeof(output)) ||
                            memcmp(before_t, tb, sizeof(tb)) || memcmp(before_l, lb, sizeof(lb))) {
                            fprintf(stderr, "FAIL direct ABI mask=0x%" PRIx64 " id=%d shared=%d mode=%d c=%d stride=%td\n", failure, id, shared, mode, c, stride);
                            return 1;
                        }
                        count++;
                    }
                }
    printf("PASS direct numerical ABI high32 int contamination cases=%" PRIu64 "\n", count);
    return 0;
}

#ifdef FRD_HEVC_BENCH_RELEASE
/* 单独 release 才编译该形态。固定规模，计时外逐池输出校验及 checksum。 */
enum { WARM = 65536, BATCHES = 31, CALLS = 262144 };
static int compare_u64(const void *a, const void *b)
{
    uint64_t x = *(const uint64_t *)a, y = *(const uint64_t *)b;
    return (x > y) - (x < y);
}
static void invoke(const HEVCPredContext *ctx, uint8_t *dst, const uint8_t *t,
                   const uint8_t *l, int mode, int c)
{
    if (mode) ctx->pred_angular[2](dst, t, l, 32, c, mode);
    else ctx->pred_planar[2](dst, t, l, 32);
}
static int benchmark(void)
{
    HEVCPredContext original = {0}, actual = {0};
    _Alignas(16) uint8_t tops[PATTERNS][REF_BYTES], lefts[PATTERNS][REF_BYTES];
    _Alignas(16) uint8_t expected[PATTERNS][512], output[512];
    static const int modes[] = {0,2,10,11,12,17,18,19,25,26,27,33,34};
    int misses = 0;
    av_force_cpu_flags(0); frd_hevc_original_init(&original, 8);
    av_force_cpu_flags(AV_CPU_FLAG_NEON); ff_hevc_pred_init(&actual, 8);
    for (int id = 0; id < PATTERNS; id++) pattern(tops[id]+16, lefts[id]+16, id);
    for (int run = 0; run < 3; run++)
        for (size_t mi = 0; mi < sizeof(modes)/sizeof(modes[0]); mi++)
            for (int c = 0; c < (modes[mi] ? 2 : 1); c++) {
                int mode = modes[mi], angle = mode ? angles[mode-2] : 0;
                int mathematical = !mode || (angle && angle != 32 && angle != -32) || (!c && (mode == 10 || mode == 26));
                uint64_t times[2][BATCHES], checksum = 0;
                for (int id = 0; id < PATTERNS; id++) {
                    memset(expected[id], 0xa5, 512);
                    if (mode) model_angular(expected[id], tops[id]+16, lefts[id]+16, 32, c, mode);
                    else model_planar(expected[id], tops[id]+16, lefts[id]+16, 32);
                    for (int which = 0; which < 2; which++) {
                        memset(output, 0xa5, 512);
                        invoke(which ? &actual : &original, output, tops[id]+16, lefts[id]+16, mode, c);
                        if (memcmp(expected[id], output, 512)) return 1;
                    }
                }
                for (int which = 0; which < 2; which++)
                    for (unsigned k = 0; k < WARM; k++) {
                        unsigned id = k % PATTERNS;
                        invoke(which ? &actual : &original, output, tops[id]+16, lefts[id]+16, mode, c);
                    }
                for (int batch = 0; batch < BATCHES; batch++)
                    for (int order = 0; order < 2; order++) {
                        int which = order ^ ((batch + run) & 1);
                        uint64_t begin, end;
                        memset(output, 0xa5, 512);
                        if (frd_dc_clock_ns(&begin)) return 1;
                        for (unsigned k = 0; k < CALLS; k++) {
                            unsigned id = k % PATTERNS;
                            invoke(which ? &actual : &original, output, tops[id]+16, lefts[id]+16, mode, c);
                        }
                        if (frd_dc_clock_ns(&end) || end < begin) return 1;
                        times[which][batch] = end - begin;
                        if (memcmp(output, expected[(CALLS-1)%PATTERNS], 512)) return 1;
                        for (size_t j = 0; j < 512; j++) checksum = checksum * 33 + output[j];
                    }
                qsort(times[0], BATCHES, sizeof(uint64_t), compare_u64);
                qsort(times[1], BATCHES, sizeof(uint64_t), compare_u64);
                double median_ratio = (double)times[1][15] / times[0][15];
                double p95_ratio = (double)times[1][29] / times[0][29];
                int pass = mathematical ? median_ratio < 1.0 : median_ratio <= 1.05 && p95_ratio <= 1.10;
                misses += !pass;
                printf("BENCH run=%d mode=%d c=%d math=%d warm=%d batches=%d calls=%d C_ns=%" PRIu64 "/%" PRIu64 "/%" PRIu64 " NEON_ns=%" PRIu64 "/%" PRIu64 "/%" PRIu64 " median_ratio=%.6f p95_ratio=%.6f pass=%d checksum=%" PRIu64 "\n", run+1, mode, c, mathematical, WARM, BATCHES, CALLS, times[0][15], times[0][29], times[0][30], times[1][15], times[1][29], times[1][30], median_ratio, p95_ratio, pass, checksum);
            }
    av_force_cpu_flags(-1);
    printf("BENCH complete runs=3 misses=%d seed=0x%08" PRIx32 "\n", misses, seed);
    return !!misses;
}
#endif

int main(int argc, char **argv)
{
    if (argc == 2 && !strcmp(argv[1], "--native")) {
        char *reference_argv[] = {argv[0], "--reference", NULL};
        if (frd_hevc_reference_main(2, reference_argv)) return 1;
        if (planar_minimum_guards() || abi_cases()) return 1;
        return 0;
    }
#ifdef FRD_HEVC_BENCH_RELEASE
    if (argc == 2 && !strcmp(argv[1], "--bench")) {
        if (selector()) return 1;
        return benchmark();
    }
#endif
    fputs("usage: hevc-intra16-native --native (benchmark requires separate release)\n", stderr);
    return 2;
}
