#ifndef FRD_IME_GRAPHEME_H
#define FRD_IME_GRAPHEME_H
#include <cstdint>
struct FrdImeGraphemeResult {
    bool valid = false;
    bool is_boundary = false;
    int32_t previous = 0;
    int32_t next = 0;
};
// 借用调用者 UTF-16，仅当前组合；无缓存、无日志、无 Owner/网络调用。
FrdImeGraphemeResult FrdImeGraphemeNeighbors(const char16_t *text, int32_t length, int32_t cursor) noexcept;
#endif
