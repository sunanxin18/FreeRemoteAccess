// UI 线程只导入调用期密码；临时复制 volatile wipe，不保留/打印 NAPI payload。
#include "app_bridge.h"
#include "ime_grapheme.h"
#include "session_diagnostic.h"
#include <hilog/log.h>
#include <atomic>
#include <chrono>
#include "frd_ohos_app.h"
#include "frd_ohos.h"
#include <charconv>
#include <cmath>
#include <limits>
#include <string>
#include <vector>
namespace {
uint64_t application = 0;
FrdSessionDiagnostic::FallbackState snapshotFallback;
void SessionDiagnostic(FrdSessionDiagnostic::Event event, uint64_t app, int32_t code) {
    using Event = FrdSessionDiagnostic::Event;
    const char *name = "unknown";
    switch (event) {
        case Event::Initialize: name = "initialize"; break;
        case Event::Shutdown: name = "shutdown"; break;
        case Event::Cleanup: name = "cleanup"; break;
        case Event::FallbackEntered: name = "fallback_entered"; break;
        case Event::FallbackCodeChanged: name = "fallback_code_changed"; break;
        case Event::FallbackExited: name = "fallback_exited"; break;
    }
    static std::atomic<uint64_t> sequence{0};
    auto micros = std::chrono::duration_cast<std::chrono::microseconds>(
        std::chrono::steady_clock::now().time_since_epoch()).count();
    OH_LOG_Print(LOG_APP, LOG_INFO, 0xFF00, "FrdSession",
        "producer=cpp clock=cpp_steady sequence=%{public}llu monotonic_us=%{public}lld app=%{public}llu bound=%{public}d event=%{public}s abi=%{public}d",
        static_cast<unsigned long long>(++sequence), static_cast<long long>(micros),
        static_cast<unsigned long long>(app), app != 0, name, code);
}
float pointerDensity = 0;
std::string sandbox;
struct Text {
    std::vector<uint8_t> bytes;
    ~Text() {
        for (auto &b : bytes) {
            *static_cast<volatile uint8_t *>(&b) = 0;
        }
    }
    bool Read(napi_env env, napi_value v, size_t max) {
        size_t size = 0;
        if (napi_get_value_string_utf8(env, v, nullptr, 0, &size) != napi_ok || size > max)
            return false;
        bytes.resize(size + 1);
        size_t copied = 0;
        if (napi_get_value_string_utf8(env, v, reinterpret_cast<char *>(bytes.data()), bytes.size(),
                                       &copied) != napi_ok ||
            copied != size)
            return false;
        bytes.resize(size);
        return true;
    }
};
napi_value Code(napi_env env, int32_t code) {
    napi_value v = nullptr;
    napi_create_int32(env, code, &v);
    return v;
}
bool Args(napi_env env, napi_callback_info info, napi_value *out, size_t count) {
    size_t n = count;
    return napi_get_cb_info(env, info, &n, out, nullptr, nullptr) == napi_ok && n == count;
}
bool U32(napi_env env, napi_value value, uint32_t &out) {
    double d = 0;
    if (napi_get_value_double(env, value, &d) != napi_ok || !std::isfinite(d) || d < 0 ||
        d > UINT32_MAX || std::floor(d) != d)
        return false;
    return napi_get_value_uint32(env, value, &out) == napi_ok;
}
bool U64(napi_env env, napi_value value, uint64_t &out) {
    Text t;
    if (!t.Read(env, value, 20) || t.bytes.empty())
        return false;
    auto first = reinterpret_cast<const char *>(t.bytes.data());
    auto result = std::from_chars(first, first + t.bytes.size(), out);
    return result.ec == std::errc() && result.ptr == first + t.bytes.size();
}
bool Float(napi_env env, napi_value value, float &out) {
    double d = 0;
    if (napi_get_value_double(env, value, &d) != napi_ok || !std::isfinite(d) ||
        std::abs(d) > std::numeric_limits<float>::max())
        return false;
    out = static_cast<float>(d);
    return true;
}
napi_value Initialize(napi_env env, napi_callback_info info) {
    napi_value a = nullptr;
    Text t;
    if (!Args(env, info, &a, 1) || !t.Read(env, a, 4096))
        return Code(env, -2);
    if (application)
        return Code(env, 0);
    uint64_t id = 0;
    auto c = frd_ohos_app_create(t.bytes.data(), t.bytes.size(), &id);
    if (c == 0) {
        application = id;
        sandbox.assign(reinterpret_cast<char *>(t.bytes.data()), t.bytes.size());
    }
    SessionDiagnostic(FrdSessionDiagnostic::Event::Initialize, id, c);
    return Code(env, c);
}
napi_value Shutdown(napi_env env, napi_callback_info) {
    SessionDiagnostic(FrdSessionDiagnostic::Event::Shutdown, application, 0);
    FrdAppCleanup();
    return Code(env, 0);
}
napi_value Snapshot(napi_env env, napi_callback_info) {
    uint8_t *out = nullptr;
    size_t n = 0;
    auto c = frd_ohos_app_snapshot(application, &out, &n);
    using Transition = FrdSessionDiagnostic::FallbackTransition;
    using Event = FrdSessionDiagnostic::Event;
    switch (snapshotFallback.Observe(application, c)) {
        case Transition::Entered: SessionDiagnostic(Event::FallbackEntered, application, c); break;
        case Transition::CodeChanged: SessionDiagnostic(Event::FallbackCodeChanged, application, c); break;
        case Transition::Exited: SessionDiagnostic(Event::FallbackExited, application, c); break;
        case Transition::None: break;
    }
    napi_value v = nullptr;
    if (c == 0) {
        napi_create_string_utf8(env, reinterpret_cast<char *>(out), n, &v);
        frd_ohos_app_snapshot_free(out, n);
    } else {
        napi_create_string_utf8(env, "{\"initialized\":false,\"available\":false,\"page\":0}",
                                NAPI_AUTO_LENGTH, &v);
    }
    return v;
}
napi_value Edit(napi_env env, napi_callback_info info) {
    napi_value a = nullptr;
    Text t;
    if (!Args(env, info, &a, 1) || !t.Read(env, a, 16384))
        return Code(env, -2);
    return Code(env, frd_ohos_app_edit(application, t.bytes.data(), t.bytes.size()));
}
napi_value LocalDisplay(napi_env env, napi_callback_info info) {
    napi_value a = nullptr;
    Text t;
    if (!Args(env, info, &a, 1) || !t.Read(env, a, 4096))
        return Code(env, -2);
    // 原始有符号坐标/浮点 density 保留为 JSON；Rust 严格校验，不在 NAPI 截断。
    return Code(env, frd_ohos_app_local_display(application, t.bytes.data(), t.bytes.size()));
}
napi_value ImeSource(napi_env env, napi_callback_info info) {
    napi_value a = nullptr;
    Text t;
    if (!Args(env, info, &a, 1) || !t.Read(env, a, 4096))
        return Code(env, -2);
    return Code(env, frd_ohos_app_ime_source(application, t.bytes.data(), t.bytes.size()));
}
napi_value ImeOperation(napi_env env, napi_callback_info info) {
    napi_value a = nullptr;
    Text t;
    if (!Args(env, info, &a, 1) || !t.Read(env, a, 32768))
        return Code(env, -2);
    return Code(env, frd_ohos_app_ime_operation(application, t.bytes.data(), t.bytes.size()));
}
struct Utf16Text {
    std::vector<char16_t> units;
    ~Utf16Text() { for (auto &unit : units) *static_cast<volatile char16_t *>(&unit) = 0; }
};
napi_value ImeGrapheme(napi_env env, napi_callback_info info) {
    napi_value args[2]; uint32_t cursor = 0; size_t length = 0; Utf16Text text;
    if (!Args(env, info, args, 2) || !U32(env, args[1], cursor) ||
        napi_get_value_string_utf16(env, args[0], nullptr, 0, &length) != napi_ok || length > 4096 || cursor > length) {
        napi_throw_error(env, nullptr, "ime_context_unavailable"); return nullptr;
    }
    text.units.resize(length + 1); size_t copied = 0;
    if (napi_get_value_string_utf16(env, args[0], text.units.data(), text.units.size(), &copied) != napi_ok || copied != length) {
        napi_throw_error(env, nullptr, "ime_context_unavailable"); return nullptr;
    }
    const auto result = FrdImeGraphemeNeighbors(text.units.data(), static_cast<int32_t>(length), static_cast<int32_t>(cursor));
    if (!result.valid) { napi_throw_error(env, nullptr, "ime_context_unavailable"); return nullptr; }
    napi_value object = nullptr, boundary = nullptr, previous = nullptr, next = nullptr;
    napi_create_object(env, &object); napi_get_boolean(env, result.is_boundary, &boundary);
    napi_create_int32(env, result.previous, &previous); napi_create_int32(env, result.next, &next);
    napi_set_named_property(env, object, "isBoundary", boundary); napi_set_named_property(env, object, "previous", previous);
    napi_set_named_property(env, object, "next", next); return object;
}
napi_value Password(napi_env env, napi_callback_info info) {
    napi_value a = nullptr;
    Text t;
    if (!Args(env, info, &a, 1) || !t.Read(env, a, 65536))
        return Code(env, -2);
    return Code(env, frd_ohos_app_password(application, t.bytes.data(), t.bytes.size()));
}
napi_value Action(napi_env env, napi_callback_info info) {
    napi_value a[4];
    uint32_t action = 0, flags = 0, index = 0;
    uint64_t event = 0;
    if (!Args(env, info, a, 4) || !U32(env, a[0], action) || !U64(env, a[1], event) ||
        !U32(env, a[2], flags) || !U32(env, a[3], index))
        return Code(env, -2);
    return Code(env, frd_ohos_app_action(application, action, event, flags, index));
}
napi_value Focus(napi_env env, napi_callback_info info) {
    napi_value a[3];
    bool foreground = false, focused = false, remote = false;
    if (!Args(env, info, a, 3) || napi_get_value_bool(env, a[0], &foreground) != napi_ok ||
        napi_get_value_bool(env, a[1], &focused) != napi_ok ||
        napi_get_value_bool(env, a[2], &remote) != napi_ok)
        return Code(env, -2);
    return Code(env, frd_ohos_app_focus(application, foreground, focused, remote));
}
napi_value Geometry(napi_env env, napi_callback_info info) {
    napi_value a[8];
    float density = 0;
    FrdOhosAppGeometry g{1, sizeof(FrdOhosAppGeometry), 0, 0, 0, 0, 0, 0, 1, 0, 0};
    if (!Args(env, info, a, 8) || !U32(env, a[0], g.width) || !U32(env, a[1], g.height) ||
        !U32(env, a[2], g.content_width) || !U32(env, a[3], g.content_height) ||
        !Float(env, a[4], g.scale) || !Float(env, a[5], g.pan_x) || !Float(env, a[6], g.pan_y) || !Float(env, a[7], density) || density <= 0 || density > 16)
        return Code(env, -2);
    const auto code = frd_ohos_app_geometry(application, &g);
    // 与几何在同一 UI 线程发布；后续 native pointer 意图按 FIFO 使用此转换。
    pointerDensity = code == 0 ? density : 0;
    return Code(env, code);
}
napi_value Input(napi_env env, napi_callback_info info) {
    napi_value a[11];
    FrdOhosAppInput p{};
    p.abi_version = 1;
    p.struct_size = sizeof(p);
    uint32_t wx = 0, wy = 0;
    if (!Args(env, info, a, 11) || !U64(env, a[0], p.session) || !U64(env, a[1], p.generation) ||
        !U64(env, a[2], p.revision) || !U32(env, a[3], p.kind) || !U32(env, a[4], p.flags) ||
        !Float(env, a[5], p.x) || !Float(env, a[6], p.y) || !U32(env, a[7], p.hid) ||
        !U32(env, a[8], p.modifiers) || !U32(env, a[9], wx) || !U32(env, a[10], wy) || wx > 254 ||
        wy > 254)
        return Code(env, -2);
    p.wheel_x = static_cast<int32_t>(wx) - 127;
    p.wheel_y = static_cast<int32_t>(wy) - 127;
    return Code(env, frd_ohos_app_input(application, &p));
}
napi_value CommitText(napi_env env, napi_callback_info info) {
    napi_value a[4];
    uint64_t s = 0, g = 0, r = 0;
    Text t;
    if (!Args(env, info, a, 4) || !U64(env, a[0], s) || !U64(env, a[1], g) || !U64(env, a[2], r) ||
        !t.Read(env, a[3], 16384))
        return Code(env, -2);
    return Code(env, frd_ohos_app_text(application, s, g, r, t.bytes.data(), t.bytes.size()));
}
napi_value Probe(napi_env env, napi_callback_info) {
    return Code(env, frd_ohos_app_huks_probe(application,
                                             reinterpret_cast<const uint8_t *>(sandbox.data()),
                                             sandbox.size()));
}
} // namespace
void FrdAppForeground(bool value) {
    if (application)
        frd_ohos_app_focus(application, value, 0, 0);
}
void FrdAppCleanup() {
    SessionDiagnostic(FrdSessionDiagnostic::Event::Cleanup, application, 0);
    if (application)
        frd_ohos_app_destroy(application);
    application = 0;
    pointerDensity = 0;
    sandbox.clear();
}
void RegisterFrdApp(napi_env env, napi_value exports) {
    napi_property_descriptor d[] = {
        {"initializeApp", nullptr, Initialize, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"shutdownApp", nullptr, Shutdown, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"appSnapshot", nullptr, Snapshot, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"editForm", nullptr, Edit, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"editPassword", nullptr, Password, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"appAction", nullptr, Action, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"appFocus", nullptr, Focus, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"appLocalDisplay", nullptr, LocalDisplay, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"appImeSource", nullptr, ImeSource, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"appImeOperation", nullptr, ImeOperation, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"imeGraphemeNeighbors", nullptr, ImeGrapheme, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"appGeometry", nullptr, Geometry, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"appInput", nullptr, Input, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"commitText", nullptr, CommitText, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"huksProbe", nullptr, Probe, nullptr, nullptr, nullptr, napi_default, nullptr}};
    napi_define_properties(env, exports, sizeof(d) / sizeof(d[0]), d);
}

int32_t FrdAppBindRenderer(uint64_t renderer) { return frd_ohos_app_bind_renderer(application, renderer); }

void FrdAppRemoteFocus(bool foreground, bool focused) {
    if (application) frd_ohos_app_focus(application, foreground, focused, focused);
}
int32_t FrdAppNativePointer(uint64_t local, float x, float y, uint32_t buttons, bool takeFocus, bool logical) {
    if (!application || pointerDensity <= 0) return FRD_OHOS_INVALID_ARGUMENT;
    const float factor = logical ? pointerDensity : 1.0F;
    return frd_ohos_app_native_pointer(application, local, x * factor, y * factor, buttons, takeFocus ? 1 : 0);
}
