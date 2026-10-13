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
