#ifndef FRD_OHOS_H
#define FRD_OHOS_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
/* ABI 1: 句柄和 generation 不复用。除 tick/status 外的调用同步等待渲染线程。
 * 所有错误通过返回值识别；panic 不跨 ABI。非空指针须满足调用期存储合约。
 * 本桥只验证图形生命周期，未实现或声称网络会话成功。 */
enum FrdOhosResult {
    FRD_OHOS_OK = 0, FRD_OHOS_INVALID_HANDLE = -1, FRD_OHOS_INVALID_ARGUMENT = -2,
    FRD_OHOS_STALE_GENERATION = -3, FRD_OHOS_UNSUPPORTED_PLATFORM = -4,
    FRD_OHOS_WINDOW_REFERENCE_FAILED = -5, FRD_OHOS_GPU_FAILED = -6,
    FRD_OHOS_WORKER_STOPPED = -7, FRD_OHOS_PANIC = -8,
    FRD_OHOS_EXHAUSTED = -9, FRD_OHOS_NOT_ATTACHED = -10
};
typedef struct FrdOhosPocStatus {
    uint64_t surface_generation, presented_frames, attaches, resizes, detaches;
    uint32_t width, height, attached, foreground;
    int32_t last_error;
    uint32_t reserved;
} FrdOhosPocStatus;
uint32_t frd_ohos_abi_version(void);
uint64_t frd_ohos_create(void);
int32_t frd_ohos_destroy(uint64_t engine);
/* window 是调用期有效的 OHNativeWindow。返回前 worker 取得 own ref，失败完成
 * 清理；无等待超时或逃逸的借用。成功写入 generation，失败写入 0。 */
int32_t frd_ohos_attach(uint64_t engine, void *window, uint32_t width, uint32_t height, uint64_t *out_surface_generation);
/* 0 尺寸只暂停呈现。迟到 generation 返回 STALE_GENERATION。 */
int32_t frd_ohos_resize(uint64_t engine, uint64_t surface_generation, uint32_t width, uint32_t height);
/* 同 generation 的重复 detach 幂等。成功返回前 surface/access/ref 全部释放。 */
int32_t frd_ohos_detach(uint64_t engine, uint64_t surface_generation);
int32_t frd_ohos_set_foreground(uint64_t engine, uint32_t foreground);
/* 仅开发图形 POC。bounded 非阻塞 tick，无像素数组/协议/秘密。 */
int32_t frd_ohos_poc_tick(uint64_t engine, uint64_t surface_generation);
int32_t frd_ohos_poc_status(uint64_t engine, FrdOhosPocStatus *out_status);
#ifdef __cplusplus
}
#endif
#endif
