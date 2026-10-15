#ifndef FRD_OHOS_BRIDGE_STATE_H
#define FRD_OHOS_BRIDGE_STATE_H
#include "frd_ohos.h"
#include <limits>
#include <string_view>

// UI 所有的组件 POD；worker 只返回自己的状态，不能清除桥的注册错误。
struct ComponentState {
    uint64_t engine = 0;
    uint64_t generation = 0;
    void *borrowedWindow = nullptr;
    bool nodeFocused = false;
    uint64_t focusGeneration = 0;
    uint64_t focusEpoch = 0;
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
        nodeFocused = false; focusGeneration = 0; focusEpoch = 0;
        mouseButtons = 0; touchBlocked = false;
        ticks = false;
        bridgeError = FRD_OHOS_OK;
    }
    bool CurrentWindow(void *window, int32_t queryCode, const FrdOhosPocStatus &snapshot) const {
        return remote && engine != 0 && generation != 0 && borrowedWindow != nullptr &&
            window == borrowedWindow && bridgeError == FRD_OHOS_OK && queryCode == FRD_OHOS_OK &&
            snapshot.last_error == FRD_OHOS_OK && snapshot.attached != 0 && snapshot.surface_generation == generation;
    }
    bool NativeFocus(void *window, int32_t queryCode, const FrdOhosPocStatus &snapshot) {
        if (!CurrentWindow(window, queryCode, snapshot)) return false;
        if (!nodeFocused) {
            if (focusEpoch == std::numeric_limits<uint64_t>::max()) {
                bridgeError = FRD_OHOS_EXHAUSTED; return false;
            }
            ++focusEpoch;
        }
        nodeFocused = true; focusGeneration = generation; return true;
    }
    bool NativeBlur(void *window, int32_t queryCode, const FrdOhosPocStatus &snapshot) {
        if (window != borrowedWindow || borrowedWindow == nullptr) return false;
        nodeFocused = false; focusGeneration = 0;
        return CurrentWindow(window, queryCode, snapshot);
    }
    bool Focused(uint64_t requestedId, uint64_t windowId, int32_t windowCode, bool foreground,
        int32_t queryCode, const FrdOhosPocStatus &snapshot) const {
        return requestedId != 0 && requestedId == windowId && windowCode == 0 && foreground &&
            snapshot.foreground != 0 && nodeFocused && focusGeneration == generation &&
            CurrentWindow(borrowedWindow, queryCode, snapshot);
    }
    // SurfaceId 始终为十进制字符串，不经过 JS number；拒绝空值、符号、空白与溢出。
    static bool ParseSurfaceId(std::string_view text, uint64_t &result) {
        if (text.empty() || text.size() > 20) return false;
        uint64_t value = 0;
        for (const char digit : text) {
            if (digit < '0' || digit > '9') return false;
            const auto part = static_cast<uint64_t>(digit - '0');
            if (value > (std::numeric_limits<uint64_t>::max() - part) / 10) return false;
            value = value * 10 + part;
        }
        if (value == 0) return false;
        result = value; return true;
    }
    int32_t StatusCode(int32_t queryCode, const FrdOhosPocStatus &snapshot) const {
        if (bridgeError != FRD_OHOS_OK) return bridgeError;
        return queryCode == FRD_OHOS_OK ? snapshot.last_error : queryCode;
    }
};
#endif
