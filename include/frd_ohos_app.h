#ifndef FRD_OHOS_APP_H
#define FRD_OHOS_APP_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
/* ABI1 应用端点不阻塞等待 I/O。非关键队列满返回 -11；取消/失焦/销毁可靠入控制槽。
 * password/text 指针仅借用至返回；password 不出现在 snapshot。Snapshot owned buffer
 * 必须且只能用 snapshot_free 配对一次。path 来自 Ability 私有 filesDir。
 * PresentationEvent 仅由 Rust renderer 发送，绝不允许 UI 宣告认证/首帧成功。 */
int32_t frd_ohos_app_create(const uint8_t *path, size_t length, uint64_t *out);
int32_t frd_ohos_app_destroy(uint64_t app);
int32_t frd_ohos_app_edit(uint64_t app, const uint8_t *json, size_t length);
int32_t frd_ohos_app_password(uint64_t app, const uint8_t *utf8, size_t length);
int32_t frd_ohos_app_action(uint64_t app, uint32_t action, uint64_t event, uint32_t flags,
                            uint32_t index);
int32_t frd_ohos_app_focus(uint64_t app, uint32_t foreground, uint32_t focused, uint32_t remote);
typedef struct FrdOhosAppGeometry {
    uint32_t abi_version, struct_size, width, height, x, y, content_width, content_height;
    float scale, pan_x, pan_y;
} FrdOhosAppGeometry;
typedef struct FrdOhosAppInput {
    uint32_t abi_version, struct_size;
    uint64_t session, generation, revision;
    uint32_t kind, flags;
    float x, y;
    uint32_t hid, modifiers;
    int32_t wheel_x, wheel_y;
} FrdOhosAppInput;
/* ABI1 增量：严格 JSON（abiVersion/stage/revision/windowId/phone/geometry/reason），
 * 上限 4096 字节；通常 geometry 与 reason 恰有一个非 null。
 * 唯一例外：geometry.workArea=null 且 reason="invalid_work_area" 明示工作区回退。
 * 系统 workArea 须非负且完全在 display 内；window/drawable 允许合法跨屏相交。像素已经是 px。
 * 不改变 FrdOhosAppGeometry 布局；该入口与 renderer/input viewport 独立。 */
int32_t frd_ohos_app_local_display(uint64_t app, const uint8_t *json, size_t length);
/* ABI1 增量 IME v1（不改变旧 POD）：严格 UTF-8 JSON，未知/重复字段拒绝。
 * source <=4096 字节：{abiVersion:1,state:"attached"|"detached",ticket:T}。
 * operation <=32768 字节：{abiVersion:1,ticket:T,serial:N,kind:"text",text:S}
 * 或 {abiVersion:1,ticket:T,serial:N,kind:"keyPair",key:K}，不允许混合。
 * T={windowStage:N,windowId:N,editorSerial:N,session:D,generation:D}。
 * N 为 1..9007199254740991 整数；D 为无前导零的非零 u64 十进制字符串。
 * S 为非空 UTF-8 字符串（<=16384 字节）；K 仅 Enter/Tab/Backspace/Delete/
 * ArrowLeft/ArrowRight/ArrowUp/ArrowDown/Home/End/PageUp/PageDown，无修饰。
 * serial 从 1 连续递增；editorSerial 在每个 stage/window 单调递增，不可复用。
 * 指针仅借用至返回；0 仅本地接受（过时 detached 无操作），后续状态见 snapshot.ime。
 * lastDispatchedSerial 仅表示 SessionHost command 通道接受，不是 wire/远端 ACK。
 * adapter 须等待 snapshot.ime.sourceAvailable 及 localDisplayEvidence 的实际窗口身份；
 * app_local_display 返回 0 仅表示排队，不表示 Owner 已接受或可以附着。
 * 旧 composition 未安装 IME；显式安装时旧未带 ticket 的 app_text 通路拒绝。
 * 不接收 preedit、几何 revision 或能力位；text/JSON/长度不进入快照或日志。 */
int32_t frd_ohos_app_ime_source(uint64_t app, const uint8_t *json, size_t length);
int32_t frd_ohos_app_ime_operation(uint64_t app, const uint8_t *json, size_t length);
int32_t frd_ohos_app_geometry(uint64_t app, const FrdOhosAppGeometry *geometry);
int32_t frd_ohos_app_input(uint64_t app, const FrdOhosAppInput *input);
int32_t frd_ohos_app_text(uint64_t app, uint64_t session, uint64_t generation, uint64_t revision,
                          const uint8_t *utf8, size_t length);
int32_t frd_ohos_app_snapshot(uint64_t app, uint8_t **out, size_t *length);
void frd_ohos_app_snapshot_free(uint8_t *buffer, size_t length);
/* debug only：独立 HUKS probe，没有登录成功含义；release 返回 -4。 */
int32_t frd_ohos_app_huks_probe(uint64_t app, const uint8_t *path, size_t length);
#ifdef __cplusplus
}
#endif
#endif
