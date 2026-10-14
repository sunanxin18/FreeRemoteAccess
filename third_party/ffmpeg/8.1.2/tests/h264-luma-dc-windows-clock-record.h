/* SPDX-License-Identifier: LGPL-2.1-or-later
 * 私有计时成功记录；不更改共享 owner/数值/benchmark 接口。
 */
#ifndef FRD_H264_LUMA_DC_WINDOWS_CLOCK_RECORD_H
#define FRD_H264_LUMA_DC_WINDOWS_CLOCK_RECORD_H
#include <stdint.h>
#define FRD_DC_WINDOWS_CLOCK_CAPACITY 256u
typedef struct {
    uint64_t tick, delta, ns;
} frd_dc_windows_clock_sample;
typedef struct {
    unsigned version, count;
    uint64_t frequency, origin_tick, last_tick;
    frd_dc_windows_clock_sample samples[FRD_DC_WINDOWS_CLOCK_CAPACITY];
} frd_dc_windows_clock_record;
/* 复制完整成功状态，不读取系统时钟、不改变计时状态；无 reset/bypass。 */
int frd_dc_windows_clock_read(frd_dc_windows_clock_record *out);
#endif
