/* SPDX-License-Identifier: LGPL-2.1-or-later
 * 私有纯时钟算术；不是 codec 软件解码路径。
 */
#ifndef FRD_H264_LUMA_DC_CLOCK_CONVERT_H
#define FRD_H264_LUMA_DC_CLOCK_CONVERT_H
#include <stdint.h>
#include <stddef.h>

static inline int frd_dc_ticks_to_ns(uint64_t delta, uint64_t frequency, uint64_t *out) {
    const uint64_t scale = UINT64_C(1000000000);
    if (!out || !frequency || frequency > (uint64_t)INT64_MAX) return -1;
    uint64_t whole = delta / frequency, rem = delta % frequency;
    if (whole > UINT64_MAX / scale) return -1;
    uint64_t result = whole * scale;
    uint64_t quotient = 0, remainder = 0;
    /* scale < 2^30。每步保持 remainder < frequency；完整商 < scale。
     * 比较/减法避免 remainder*2 或 remainder+rem 的中间溢出。
     */
    for (unsigned bit = 30; bit-- > 0;) {
        quotient *= 2;
        if (remainder >= frequency - remainder) {
            remainder -= frequency - remainder;
            ++quotient;
        } else {
            remainder += remainder;
        }
        if ((scale >> bit) & 1) {
            if (remainder >= frequency - rem) {
                remainder -= frequency - rem;
                ++quotient;
            } else {
                remainder += rem;
            }
        }
    }
    if (quotient > UINT64_MAX - result) return -1;
    *out = result + quotient;
    return 0;
}
#endif
