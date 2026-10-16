#include "bridge_state.h"
#include "session_diagnostic.h"
#include <cassert>
int main() {
    FrdSessionDiagnostic::FallbackState fallback;
    using Transition = FrdSessionDiagnostic::FallbackTransition;
    assert(fallback.Observe(7, 0) == Transition::None);
    assert(fallback.Observe(7, -1) == Transition::Entered);
    assert(fallback.Observe(7, -1) == Transition::None);
    assert(fallback.Observe(7, -8) == Transition::CodeChanged);
    assert(fallback.Observe(7, 0) == Transition::Exited);
    assert(fallback.Observe(7, 0) == Transition::None);
    assert(fallback.Observe(0, -1) == Transition::Entered);
    assert(fallback.Observe(8, -1) == Transition::Entered);

    ComponentState component;
    component.engine = 1;
    component.FrameRegistrationCompleted(false);
    FrdOhosPocStatus worker{};
    worker.attached = 1;
    for (int query = 0; query < 3; ++query) {
        assert(!component.ticks);
        assert(component.StatusCode(FRD_OHOS_OK, worker) == FRD_OHOS_GPU_FAILED);
    }
    // worker 查询失败也不能伪装桥已恢复；真正的新 attach 才清除桥错误。
    assert(component.StatusCode(FRD_OHOS_WORKER_STOPPED, worker) == FRD_OHOS_GPU_FAILED);
    component.BeginAttach();
    component.FrameRegistrationCompleted(true);
    assert(component.StatusCode(FRD_OHOS_OK, worker) == FRD_OHOS_OK);
    worker.last_error = FRD_OHOS_STALE_GENERATION;
    assert(component.StatusCode(FRD_OHOS_OK, worker) == FRD_OHOS_STALE_GENERATION);
    assert(component.StatusCode(FRD_OHOS_WORKER_STOPPED, worker) == FRD_OHOS_WORKER_STOPPED);
    // 未建立 engine 的生命周期注册失败仍必须能被 status 观察。
    ComponentState initial;
    initial.bridgeError = FRD_OHOS_GPU_FAILED;
    assert(initial.StatusCode(FRD_OHOS_OK, FrdOhosPocStatus{}) == FRD_OHOS_GPU_FAILED);
    // 焦点只来自匹配当前窗口/代际的真实 Native 回调，尺寸与轮询不产生证明。
    ComponentState remote;
    int windowA = 0, windowB = 0;
    remote.remote = true; remote.engine = 7; remote.generation = 11;
    remote.borrowedWindow = &windowA;
    FrdOhosPocStatus current{};
    current.surface_generation = 11; current.attached = 1; current.foreground = 1;
    assert(!remote.Focused(9007199254740993ULL, 9007199254740993ULL, 0, true, 0, current));
    assert(!remote.NativeFocus(&windowB, 0, current));
    assert(remote.NativeFocus(&windowA, 0, current));
    assert(remote.Focused(9007199254740993ULL, 9007199254740993ULL, 0, true, 0, current));
    assert(!remote.Focused(9007199254740992ULL, 9007199254740993ULL, 0, true, 0, current));
    assert(!remote.Focused(1, 1, -1, true, 0, current));
    assert(!remote.Focused(1, 1, 0, false, 0, current));
    current.foreground = 0;
    assert(!remote.Focused(1, 1, 0, true, 0, current));
    assert(remote.nodeFocused); // 窗口活动与节点焦点正交。
    current.foreground = 1;
    auto stale = current; stale.surface_generation = 12;
    assert(!remote.Focused(1, 1, 0, true, 0, stale));
    assert(!remote.NativeFocus(&windowA, 0, stale));
    assert(!remote.NativeBlur(&windowB, 0, current));
    assert(remote.nodeFocused);
    assert(!remote.NativeBlur(&windowA, 0, stale));
    assert(!remote.nodeFocused); // 旧组件清自己的记录，不获得撤销当前 Owner 的资格。
    assert(remote.NativeFocus(&windowA, 0, current));
    remote.bridgeError = FRD_OHOS_GPU_FAILED;
    assert(!remote.Focused(1, 1, 0, true, 0, current));
    remote.BeginAttach();
    assert(!remote.nodeFocused && remote.focusGeneration == 0);
    remote.generation = 12;
    assert(!remote.Focused(1, 1, 0, true, 0, stale));
    assert(remote.NativeFocus(&windowA, 0, stale));
    assert(remote.NativeBlur(&windowA, 0, stale));
    assert(!remote.Focused(1, 1, 0, true, 0, stale));
    assert(!remote.NativeFocus(&windowA, FRD_OHOS_WORKER_STOPPED, stale));
    stale.last_error = FRD_OHOS_GPU_FAILED;
    assert(!remote.NativeFocus(&windowA, 0, stale));
    remote.BeginAttach(); remote.generation = 12; stale.last_error = 0;
    remote.focusEpoch = UINT64_MAX;
    assert(!remote.NativeFocus(&windowA, 0, stale));
    assert(remote.bridgeError == FRD_OHOS_EXHAUSTED && !remote.nodeFocused);
    uint64_t id = 0;
    assert(ComponentState::ParseSurfaceId("9007199254740993", id) && id == 9007199254740993ULL);
    assert(ComponentState::ParseSurfaceId("18446744073709551615", id) && id == UINT64_MAX);
    for (const auto text : {"", "0", "-1", "+1", "1 ", " 1", "1.0", "18446744073709551616", "100000000000000000000"}) {
        assert(!ComponentState::ParseSurfaceId(text, id));
    }

}
