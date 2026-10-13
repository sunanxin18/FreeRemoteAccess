#include "bridge_state.h"
#include <cassert>
int main() {
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
}
