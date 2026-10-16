/* SPDX-License-Identifier: LGPL-2.1-or-later
 * 私有新32路径 GREEN 准备；复用唯一 case/model，不复制候选算法。
 * --native 汇合像素、最小读取和直接 ABI 用例；是否通过须由实际运行证明。
 * ABI helper 由旧只读 .S 预处理同时改名；本用例第7参恒为1，只直调32核。
 */
#if defined(FRD_HEVC32_ORIGINAL_TU) || defined(FRD_HEVC32_SELECTOR_RED)
#error native tests must not contain the original TU or RED marker
#endif
#if defined(_WIN32)
#error private native guard/ABI closure unavailable for Windows ARM64
#endif
#define FRD_HEVC32_REQUIRE_NATIVE 1
#define main frd_hevc32_reference_main
#include "hevc-angular32-reference.c"
#undef main
#include "h264-luma-dc-platform.h"

void ff_hevc_pred_angular_32_v_8_neon(uint8_t *, const uint8_t *, const uint8_t *, ptrdiff_t, int, int);

uint64_t frd_hevc_angular32_abi(uint8_t *, const uint8_t *, const uint8_t *,
                              ptrdiff_t, int, int, int);

/* 数值负控与边界写负控分开诊断；完整8192B比较含全部行间和首尾 canary。 */
static const char *destination_region(size_t offset, size_t start, ptrdiff_t stride)
{
    for (int y = 0; y < N; y++) {
        ptrdiff_t row = (ptrdiff_t)start + y * stride;
        if ((ptrdiff_t)offset >= row && (ptrdiff_t)offset < row + N) return "pixel";
    }
    return "canary";
}
static int direct_vertical_cases(int use_abi)
{
    _Alignas(16) uint8_t tb[REF_BYTES], lb[REF_BYTES];
    _Alignas(16) uint8_t expected[DST_BYTES], output[DST_BYTES];
    uint8_t before_t[REF_BYTES], before_l[REF_BYTES];
    uint64_t count = 0;
    for (int id = 0; id < PATTERNS; id++)
        for (int shared = 0; shared < 2; shared++)
            for (size_t si = 0; si < sizeof(strides) / sizeof(strides[0]); si++)
                for (int mode = 18; mode <= 34; mode++)
                    for (int c = 0; c < 3; c++) {
                        uint8_t *top = tb + 17, *left = lb + 19;
                        size_t start = destination_start(strides[si], 7);
                        memset(tb, 0x9b, sizeof(tb)); memset(lb, 0x6d, sizeof(lb));
                        pattern(top, left, id);
                        if (shared) left = top;
                        memcpy(before_t, tb, sizeof(tb)); memcpy(before_l, lb, sizeof(lb));
                        memset(expected, 0xa5, sizeof(expected)); memset(output, 0xa5, sizeof(output));
                        model_angular(expected + start, top, left, strides[si], c, mode);
                        uint64_t failure = 0;
                        if (use_abi)
                            failure = frd_hevc_angular32_abi(output + start, top, left, strides[si], c, mode, 1);
                        else
                            ff_hevc_pred_angular_32_v_8_neon(output + start, top, left, strides[si], c, mode);
                        if (failure || memcmp(expected, output, sizeof(output)) ||
                            memcmp(before_t, tb, sizeof(tb)) || memcmp(before_l, lb, sizeof(lb))) {
                            size_t offset = 0;
                            while (offset < sizeof(output) && expected[offset] == output[offset]) offset++;
                            fprintf(stderr, "FAIL direct vertical32 abi=%d mask=0x%" PRIx64
                                    " id=%d shared=%d mode=%d c=%d stride=%td region=%s offset=%zu expected=%d actual=%d input_changed=%d\n",
                                    use_abi, failure, id, shared, mode, c, strides[si],
                                    offset < sizeof(output) ? destination_region(offset, start, strides[si]) : "none", offset,
                                    offset < sizeof(output) ? expected[offset] : -1,
                                    offset < sizeof(output) ? output[offset] : -1,
                                    !!memcmp(before_t, tb, sizeof(tb)) || !!memcmp(before_l, lb, sizeof(lb)));
                            return 1;
                        }
                        count++;
                    }
    if (count != 37536) { fputs("FAIL incomplete direct32 matrix\n", stderr); return 1; }
    printf("PASS direct vertical32 %s cases=%" PRIu64 " full8192-dst/full128-input comparison\n",
           use_abi ? "numerical ABI high32 int contamination" : "pixels/input (not ABI proof)", count);
    return 0;
}

/* 最小连续读取区间来自同一独立模型的实际 sample，未使用的边指向不可读页。
 * 两侧分别将区间的首/末 byte 紧贴保护页；fraction0 不记录第二样本。
 * dst 用8192B完整 canary，避免假定所有目标系统页尺寸可容纳7999B footprint。
 */
static uint8_t *guard_reference(frd_dc_guard *guard, int side, int low, int high)
{
    if (high < low) return guard->base + guard->page_size / 2; /* 含邻近负下标均不可读。 */
    return guard->page + (side ? (ptrdiff_t)guard->page_size - 1 - high : -low);
}
static int minimum_reference_guards(void)
{
    frd_dc_guard tg = {0}, lg = {0};
    uint8_t *before_t = NULL, *before_l = NULL;
    HEVCPredContext actual = {0};
    int result = 1;
    uint64_t count = 0;
    _Alignas(16) uint8_t tb[REF_BYTES], lb[REF_BYTES];
    _Alignas(16) uint8_t expected[DST_BYTES], output[DST_BYTES];
    av_force_cpu_flags(AV_CPU_FLAG_NEON); ff_hevc_pred_init(&actual, 8);
    if (actual.pred_angular[3] != ff_hevc_pred_angular_32_8_dispatch_aarch64) goto cleanup;
    if (frd_dc_guard_create(&tg) || frd_dc_guard_create(&lg)) goto cleanup;
    before_t = malloc(tg.page_size); before_l = malloc(lg.page_size);
    if (!before_t || !before_l) goto cleanup;
    for (int side = 0; side < 2; side++)
        for (int shared = 0; shared < 2; shared++)
            for (int id = 0; id < PATTERNS; id++)
                for (size_t si = 0; si < sizeof(strides) / sizeof(strides[0]); si++)
                    for (int mode = 18; mode <= 34; mode++)
                        for (int c = 0; c < 3; c++) {
                            uint8_t *source_t = tb + 16, *source_l = lb + 16;
                            uint8_t *top, *left;
                            reference_ranges ranges;
                            size_t start = destination_start(strides[si], 7);
                            memset(tb, 0x9b, sizeof(tb)); memset(lb, 0x6d, sizeof(lb));
                            pattern(source_t, source_l, id);
                            if (shared) source_l = source_t;
                            memset(expected, 0xa5, sizeof(expected));
                            model_angular_ranges(expected + start, source_t, source_l,
                                                 strides[si], c, mode, &ranges);
                            if (shared) {
                                if (ranges.low[1] < ranges.low[0]) ranges.low[0] = ranges.low[1];
                                if (ranges.high[1] > ranges.high[0]) ranges.high[0] = ranges.high[1];
                                ranges.low[1] = 64; ranges.high[1] = -2;
                            }
                            if (frd_dc_guard_access(&tg, FRD_DC_READ_WRITE) ||
                                frd_dc_guard_access(&lg, FRD_DC_READ_WRITE)) goto cleanup;
                            memset(tg.page, 0x9b, tg.page_size); memset(lg.page, 0x6d, lg.page_size);
                            top = guard_reference(&tg, side, ranges.low[0], ranges.high[0]);
                            left = guard_reference(&lg, side, ranges.low[1], ranges.high[1]);
                            for (int k = ranges.low[0]; k <= ranges.high[0]; k++) top[k] = source_t[k];
                            for (int k = ranges.low[1]; k <= ranges.high[1]; k++) left[k] = source_l[k];
                            if (shared) left = top;
                            memcpy(before_t, tg.page, tg.page_size); memcpy(before_l, lg.page, lg.page_size);
                            if (frd_dc_guard_access(&tg, FRD_DC_READ_ONLY) ||
                                frd_dc_guard_access(&lg, FRD_DC_READ_ONLY)) goto cleanup;
                            for (int which = 0; which < 2; which++) {
                                memset(output, 0xa5, sizeof(output));
                                if (which)
                                    ff_hevc_pred_angular_32_v_8_neon(output + start, top, left, strides[si], c, mode);
                                else
                                    actual.pred_angular[3](output + start, top, left, strides[si], c, mode);
                                if (memcmp(expected, output, sizeof(output)) ||
                                    memcmp(before_t, tg.page, tg.page_size) || memcmp(before_l, lg.page, lg.page_size)) {
                                    fprintf(stderr, "FAIL vertical32 minimum RO guards side=%d shared=%d id=%d stride=%td mode=%d c=%d direct=%d bounds=%d:%d/%d:%d\n",
                                            side, shared, id, strides[si], mode, c, which,
                                            ranges.low[0], ranges.high[0], ranges.low[1], ranges.high[1]);
                                    goto cleanup;
                                }
                                count++;
                            }
                        }
    if (count != 150144) { fputs("FAIL incomplete guard32 matrix\n", stderr); goto cleanup; }
    printf("PASS vertical32 minimum RO reference guards callback_cases=%" PRIu64
           " dst8192-canary (not dst guard-page or ABI proof)\n", count);
    result = 0;
cleanup:
    av_force_cpu_flags(-1);
    free(before_t); free(before_l);
    if (frd_dc_guard_destroy(&lg)) result = 1;
    if (frd_dc_guard_destroy(&tg)) result = 1;
    return result;
}
int main(int argc, char **argv)
{
    if (argc == 2 && !strcmp(argv[1], "--native")) {
        char *reference_argv[] = {argv[0], "--reference", NULL};
        if (frd_hevc32_reference_main(2, reference_argv)) return 1;
        if (direct_vertical_cases(0) || minimum_reference_guards() || direct_vertical_cases(1)) return 1;
        return 0;
    }
    /* 独立固定门使负控能区分正常退出的数值/canary失败与保护页 signal。 */
    if (argc == 2 && !strcmp(argv[1], "--direct")) return direct_vertical_cases(0);
    if (argc == 2 && !strcmp(argv[1], "--abi")) return direct_vertical_cases(1);
    if (argc == 2 && !strcmp(argv[1], "--guards")) return minimum_reference_guards();
    fputs("usage: hevc-angular32-native --native | --direct | --abi | --guards\n", stderr);
    return 2;
}
