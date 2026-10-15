#include "ime_grapheme.h"
#include <memory>
#include <unicode/ubrk.h>
namespace {
bool High(char16_t c) { return c >= 0xD800 && c <= 0xDBFF; }
bool Low(char16_t c) { return c >= 0xDC00 && c <= 0xDFFF; }
struct BreakCloser { void operator()(UBreakIterator *iterator) const noexcept { if (iterator) ubrk_close(iterator); } };
}
FrdImeGraphemeResult FrdImeGraphemeNeighbors(const char16_t *text, int32_t length, int32_t cursor) noexcept {
    if (length < 0 || length > 4096 || cursor < 0 || cursor > length || (!text && length != 0)) return {};
    for (int32_t i = 0; i < length; ++i) {
        if (High(text[i])) { if (++i >= length || !Low(text[i])) return {}; }
        else if (Low(text[i])) return {};
    }
    if (cursor > 0 && cursor < length && High(text[cursor - 1]) && Low(text[cursor])) return {};
    if (length == 0) return {true, true, 0, 0};
    static_assert(sizeof(UChar) == sizeof(char16_t));
    UErrorCode error = U_ZERO_ERROR;
    const std::unique_ptr<UBreakIterator, BreakCloser> iterator(
        ubrk_open(UBRK_CHARACTER, "root", reinterpret_cast<const UChar *>(text), length, &error));
    if (U_FAILURE(error) || !iterator) return {};
    const bool boundary = ubrk_isBoundary(iterator.get(), cursor);
    const int32_t previous = cursor == 0 ? 0 : ubrk_preceding(iterator.get(), cursor);
    const int32_t next = cursor == length ? length : ubrk_following(iterator.get(), cursor);
    if (previous < 0 || previous > cursor || next < cursor || next > length) return {};
    return {true, boundary, previous, next};
}
