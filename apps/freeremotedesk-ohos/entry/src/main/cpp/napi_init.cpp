// UI 线程只访问 NAPI/XComponent 与 POD，Rust worker 独占 native window/GPU。
#include "napi/native_api.h"
#include "ace/xcomponent/native_interface_xcomponent.h"
#include "hilog/log.h"
#include "frd_ohos.h"
#include "bridge_state.h"
#include "app_bridge.h"
#include <limits>
#include <cstddef>
#include <memory>
#include <unordered_map>
#include <unordered_set>
#include <string>

static_assert(sizeof(FrdOhosPocStatus) == 64, "ABI 1 的状态 POD 必须为 64 字节");
static_assert(offsetof(FrdOhosPocStatus, last_error) == 56, "ABI 1 字段偏移必须匹配 Rust");

namespace {
// 所有入口由 ArkUI/NAPI 的 UI 线程调用，worker 不访问此表。
std::unordered_map<OH_NativeXComponent *, std::unique_ptr<ComponentState>> components;
std::unordered_set<napi_env> cleanupEnvironments;
bool foreground = true;
uint64_t remoteEngine = 0;
FrdOhosPocStatus lastStatus{};
int32_t lastCode = FRD_OHOS_OK;
void Log(const char *event, int32_t code, const ComponentState &state) {
    OH_LOG_Print(LOG_APP, code == 0 ? LOG_INFO : LOG_ERROR, 0xFF00, "FrdOhos",
        "%{public}s code=%{public}d engine=%{public}llu generation=%{public}llu", event, code,
        static_cast<unsigned long long>(state.engine), static_cast<unsigned long long>(state.generation));
}
ComponentState *Find(OH_NativeXComponent *component) {
    auto found = components.find(component); return found == components.end() ? nullptr : found->second.get();
}
bool Extent(OH_NativeXComponent *component, void *window, uint32_t &width, uint32_t &height) {
    uint64_t w=0, h=0;
    if (OH_NativeXComponent_GetXComponentSize(component, window, &w, &h) != OH_NATIVEXCOMPONENT_RESULT_SUCCESS ||
        w > std::numeric_limits<uint32_t>::max() || h > std::numeric_limits<uint32_t>::max()) return false;
    width=static_cast<uint32_t>(w); height=static_cast<uint32_t>(h); return true;
}
void Frame(OH_NativeXComponent *component, uint64_t, uint64_t) {
    auto *state=Find(component);
    if (state && !state->remote && state->ticks && foreground) frd_ohos_poc_tick(state->engine, state->generation);
}
void Created(OH_NativeXComponent *component, void *window) {
    auto *state=Find(component); if (!state) return;
    state->BeginAttach();
    if (!state->remote && !FRD_GRAPHICS_POC_ENABLED) { state->bridgeError=lastCode=FRD_OHOS_UNSUPPORTED_PLATFORM; return; }
    uint32_t width=0, height=0;
    if (!Extent(component, window, width, height)) { state->bridgeError=lastCode=FRD_OHOS_INVALID_ARGUMENT; return; }
    if (state->remote) {
        if (remoteEngine == 0) remoteEngine = frd_ohos_create();
        state->engine = remoteEngine;
    } else if (state->engine == 0) state->engine=frd_ohos_create();
    if (state->engine == 0) { state->bridgeError=lastCode=FRD_OHOS_WORKER_STOPPED; return; }
    if (state->remote) {
        const auto bindCode = FrdAppBindRenderer(state->engine);
        if (bindCode != FRD_OHOS_OK) { state->bridgeError=lastCode=bindCode; return; }
    }
    const auto foregroundCode=frd_ohos_set_foreground(state->engine, foreground ? 1 : 0);
    if (foregroundCode != FRD_OHOS_OK) { state->bridgeError=lastCode=foregroundCode; return; }
    // 同步等待，没有超时；callback 返回前取得 own ref 或完成全部失败清理。
    uint64_t generation=0;
    const auto code=frd_ohos_attach(state->engine, window, width, height, &generation);
    lastCode=code; state->generation=generation;
    if (code == FRD_OHOS_OK) {
        state->borrowedWindow=window;
        state->FrameRegistrationCompleted(OH_NativeXComponent_RegisterOnFrameCallback(component, Frame) == OH_NATIVEXCOMPONENT_RESULT_SUCCESS);
        lastCode=state->StatusCode(FRD_OHOS_OK, FrdOhosPocStatus{});
    }
    Log("attach", lastCode, *state);
}
void Changed(OH_NativeXComponent *component, void *window) {
    auto *state=Find(component); if (!state || window != state->borrowedWindow || state->generation == 0) return;
    uint32_t width=0, height=0;
    if (!Extent(component, window, width, height)) return;
    lastCode=frd_ohos_resize(state->engine, state->generation, width, height);
    Log("resize", lastCode, *state);
}
void Destroyed(OH_NativeXComponent *component, void *window) {
    auto *state=Find(component); if (!state || (state->borrowedWindow && window != state->borrowedWindow)) return;
    state->ticks=false;
    OH_NativeXComponent_UnregisterOnFrameCallback(component);
    lastCode=state->generation ? frd_ohos_detach(state->engine, state->generation) : FRD_OHOS_OK;
    const auto statusCode=frd_ohos_poc_status(state->engine, &lastStatus);
    lastCode=state->StatusCode(lastCode == FRD_OHOS_OK ? statusCode : lastCode, lastStatus);
    // detach 返回前已暂停/释放 Surface/清空访问/unref，然后 join worker。
    Log("detach", lastCode, *state);
    if (!state->remote) {
        const auto destroyCode=frd_ohos_destroy(state->engine);
        Log("destroy", destroyCode, *state);
        state->engine=0;
    }
    state->generation=0; state->borrowedWindow=nullptr;
    state->bridgeError=FRD_OHOS_OK;
    // 同一 XComponent 在系统重建 Surface 时仍可再次 Created，保留已清零 POD。
}
void RemoteBlur(OH_NativeXComponent *component, void *window) {
    auto *state=Find(component); if (!state || !state->remote || window != state->borrowedWindow) return;
    state->mouseButtons=0; FrdAppRemoteFocus(foreground,false);
}
void RemoteFocus(OH_NativeXComponent *component, void *window) {
    auto *state=Find(component); if (!state || !state->remote || window != state->borrowedWindow) return;
    FrdOhosPocStatus current{};
    if (frd_ohos_poc_status(state->engine,&current) != FRD_OHOS_OK || current.surface_generation != state->generation || !current.attached) return;
    FrdAppRemoteFocus(foreground,true);
}
void Touch(OH_NativeXComponent *component, void *window) {
    auto *state=Find(component); if (!state || !state->remote || window != state->borrowedWindow || !state->generation) return;
    OH_NativeXComponent_TouchEvent event{};
    if (OH_NativeXComponent_GetTouchEvent(component,window,&event) != OH_NATIVEXCOMPONENT_RESULT_SUCCESS) { RemoteBlur(component,window); return; }
    if (event.numPoints > 1 || event.type == OH_NATIVEXCOMPONENT_CANCEL) {
        state->touchBlocked=event.numPoints>1; RemoteBlur(component,window); return;
    }
    if (event.type != OH_NATIVEXCOMPONENT_DOWN && event.type != OH_NATIVEXCOMPONENT_UP && event.type != OH_NATIVEXCOMPONENT_MOVE) { RemoteBlur(component,window); return; }
    if (state->touchBlocked) {
        if (event.type == OH_NATIVEXCOMPONENT_UP && event.numPoints <= 1) state->touchBlocked=false;
        return;
    }
    // Native TouchEvent 的本地坐标来自 XComponent 物理表面；MouseEvent 则由 SDK 明确为 vp。
    lastCode=FrdAppNativePointer(state->generation,event.x,event.y,event.type == OH_NATIVEXCOMPONENT_UP ? 0 : 1,event.type == OH_NATIVEXCOMPONENT_DOWN,false);
}
void Mouse(OH_NativeXComponent *component, void *window) {
    auto *state=Find(component); if (!state || !state->remote || window != state->borrowedWindow || !state->generation) return;
    OH_NativeXComponent_MouseEvent event{};
    if (OH_NativeXComponent_GetMouseEvent(component,window,&event) != OH_NATIVEXCOMPONENT_RESULT_SUCCESS || event.action == OH_NATIVEXCOMPONENT_MOUSE_CANCEL) { RemoteBlur(component,window); return; }
    const auto button=event.button == OH_NATIVEXCOMPONENT_LEFT_BUTTON ? 1U : event.button == OH_NATIVEXCOMPONENT_MIDDLE_BUTTON ? 2U : event.button == OH_NATIVEXCOMPONENT_RIGHT_BUTTON ? 4U : 0U;
    if (event.action == OH_NATIVEXCOMPONENT_MOUSE_PRESS) state->mouseButtons |= button;
    if (event.action == OH_NATIVEXCOMPONENT_MOUSE_RELEASE) state->mouseButtons &= ~button;
    lastCode=FrdAppNativePointer(state->generation,event.x,event.y,state->mouseButtons,event.action == OH_NATIVEXCOMPONENT_MOUSE_PRESS,true);
}
void Hover(OH_NativeXComponent *, bool) {}
OH_NativeXComponent_MouseEvent_Callback mouseCallbacks{Mouse,Hover};
OH_NativeXComponent_Callback callbacks{Created, Changed, Destroyed, Touch};
napi_value PocAvailable(napi_env env, napi_callback_info) {
    napi_value result=nullptr; napi_get_boolean(env, FRD_GRAPHICS_POC_ENABLED != 0, &result); return result;
}
napi_value Abi(napi_env env, napi_callback_info) {
    napi_value result=nullptr; napi_create_uint32(env, frd_ohos_abi_version(), &result); return result;
}
napi_value Foreground(napi_env env, napi_callback_info info) {
    size_t argc=1; napi_value argument=nullptr; bool value=false;
    if (napi_get_cb_info(env, info, &argc, &argument, nullptr, nullptr) != napi_ok || argc != 1 || napi_get_value_bool(env, argument, &value) != napi_ok) {
        napi_throw_type_error(env, nullptr, "前台状态必须是布尔值"); return nullptr;
    }
    foreground=value; FrdAppForeground(value); int32_t code=0;
    for (const auto &item : components) if (item.second->engine) {
        const auto result=frd_ohos_set_foreground(item.second->engine, value ? 1 : 0); if (result != 0) code=result;
    }
    napi_value result=nullptr; napi_create_int32(env, code, &result); return result;
}
void Number(napi_env env, napi_value object, const char *key, int32_t value) {
    napi_value result=nullptr; napi_create_int32(env, value, &result); napi_set_named_property(env, object, key, result);
}
void Counter(napi_env env, napi_value object, const char *key, uint64_t value) {
    const auto text=std::to_string(value); napi_value result=nullptr; napi_create_string_utf8(env, text.c_str(), text.size(), &result); napi_set_named_property(env, object, key, result);
}
void Bool(napi_env env, napi_value object, const char *key, bool value) {
    napi_value result=nullptr; napi_get_boolean(env, value, &result); napi_set_named_property(env, object, key, result);
}
napi_value Status(napi_env env, napi_callback_info) {
    auto snapshot=lastStatus; auto code=lastCode;
    const ComponentState *selected=nullptr;
    for (const auto &item : components) if (item.second->engine) { selected=item.second.get(); break; }
    // 生命周期注册失败可发生在创建 engine 之前，仍需提供持久的可辨识错误。
    if (!selected) for (const auto &item : components) if (item.second->bridgeError != FRD_OHOS_OK) { selected=item.second.get(); break; }
    if (selected) {
        const auto queryCode=selected->engine ? frd_ohos_poc_status(selected->engine, &snapshot) : FRD_OHOS_OK;
        code=selected->StatusCode(queryCode, snapshot);
    }
    napi_value result=nullptr; napi_create_object(env, &result);
    Number(env,result,"code",code); Number(env,result,"width",snapshot.width); Number(env,result,"height",snapshot.height);
    Counter(env,result,"generation",snapshot.surface_generation); Counter(env,result,"frames",snapshot.presented_frames);
    Counter(env,result,"attaches",snapshot.attaches); Counter(env,result,"resizes",snapshot.resizes); Counter(env,result,"detaches",snapshot.detaches);
    Bool(env,result,"attached",snapshot.attached != 0); Bool(env,result,"foreground",snapshot.foreground != 0);
    return result;
}
void Cleanup(void *environment) {
    FrdAppCleanup();
    cleanupEnvironments.erase(static_cast<napi_env>(environment));
    // env 清理时不访问可能已失效的 XComponent，只 join native worker。
    for (auto &item : components) if (item.second->engine) frd_ohos_destroy(item.second->engine);
    remoteEngine = 0;
    components.clear();
}
napi_value Init(napi_env env, napi_value exports) {
    RegisterFrdApp(env,exports);
    if (cleanupEnvironments.insert(env).second) napi_add_env_cleanup_hook(env,Cleanup,env);
    napi_property_descriptor descriptors[]={{"abiVersion",nullptr,Abi,nullptr,nullptr,nullptr,napi_default,nullptr},
        {"pocAvailable",nullptr,PocAvailable,nullptr,nullptr,nullptr,napi_default,nullptr},
        {"setForeground",nullptr,Foreground,nullptr,nullptr,nullptr,napi_default,nullptr},
        {"pocStatus",nullptr,Status,nullptr,nullptr,nullptr,napi_default,nullptr}};
    napi_define_properties(env,exports,sizeof(descriptors)/sizeof(descriptors[0]),descriptors);
    bool has=false; napi_value object=nullptr; OH_NativeXComponent *component=nullptr;
    // ArkTS 直接 import libentry.so 不必带 XComponent 对象。
    if (napi_has_named_property(env,exports,OH_NATIVE_XCOMPONENT_OBJ,&has) == napi_ok && has &&
        napi_get_named_property(env,exports,OH_NATIVE_XCOMPONENT_OBJ,&object) == napi_ok &&
        napi_unwrap(env,object,reinterpret_cast<void **>(&component)) == napi_ok && component) {
        if (components.find(component) == components.end()) components.emplace(component,std::make_unique<ComponentState>());
        auto *state=Find(component);
        char componentId[OH_XCOMPONENT_ID_LEN_MAX + 1]{};
        uint64_t componentIdSize = sizeof(componentId);
        if (OH_NativeXComponent_GetXComponentId(component, componentId, &componentIdSize) != OH_NATIVEXCOMPONENT_RESULT_SUCCESS) {
            state->bridgeError=lastCode=FRD_OHOS_INVALID_ARGUMENT; return exports;
        }
        state->remote = std::string(componentId) == "frdRemoteSurface";
        if (!state->remote && std::string(componentId) != "frdPocSurface") {
            state->bridgeError=lastCode=FRD_OHOS_INVALID_ARGUMENT; return exports;
        }
        if (state->remote && (OH_NativeXComponent_RegisterMouseEventCallback(component,&mouseCallbacks) != OH_NATIVEXCOMPONENT_RESULT_SUCCESS ||
            OH_NativeXComponent_RegisterFocusEventCallback(component,RemoteFocus) != OH_NATIVEXCOMPONENT_RESULT_SUCCESS ||
            OH_NativeXComponent_RegisterBlurEventCallback(component,RemoteBlur) != OH_NATIVEXCOMPONENT_RESULT_SUCCESS)) {
            state->bridgeError=lastCode=FRD_OHOS_INVALID_ARGUMENT; return exports;
        }
        if (OH_NativeXComponent_RegisterCallback(component,&callbacks) != OH_NATIVEXCOMPONENT_RESULT_SUCCESS) {
            state->bridgeError=lastCode=FRD_OHOS_GPU_FAILED;
            Log("register-lifecycle", lastCode, *state);
        }
    }
    return exports;
}
napi_module module{1,0,nullptr,Init,"entry",nullptr,{0}};
}
extern "C" __attribute__((constructor)) void RegisterFreeRemoteDeskModule() { napi_module_register(&module); }
