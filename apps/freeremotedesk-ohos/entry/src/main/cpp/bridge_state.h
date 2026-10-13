#ifndef FRD_OHOS_BRIDGE_STATE_H
#define FRD_OHOS_BRIDGE_STATE_H
#include "frd_ohos.h"

// UI 所有的组件 POD；worker 只返回自己的状态，不能清除桥的注册错误。
struct ComponentState {
    uint64_t engine = 0;
    uint64_t generation = 0;
    void *borrowedWindow = nullptr;
    bool ticks = false;
    bool remote = false;
    bool touchBlocked = false;
    uint32_t mouseButtons = 0;
    int32_t bridgeError = FRD_OHOS_OK;

    void FrameRegistrationCompleted(bool registered) {
        ticks = registered;
        if (!registered) bridgeError = FRD_OHOS_GPU_FAILED;
    }
    void BeginAttach() {
        mouseButtons = 0; touchBlocked = false;
        ticks = false;
        bridgeError = FRD_OHOS_OK;
    }
    int32_t StatusCode(int32_t queryCode, const FrdOhosPocStatus &snapshot) const {
        if (bridgeError != FRD_OHOS_OK) return bridgeError;
        return queryCode == FRD_OHOS_OK ? snapshot.last_error : queryCode;
    }
};
#endif
