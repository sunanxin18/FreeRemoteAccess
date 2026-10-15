// Synthetic public-ICU contract runner. No typed text, payload or lengths are logged.
#include "ime_grapheme.h"
#include <cstdio>
#include <string>
#include <vector>
struct Fixture { std::u16string text; std::vector<int32_t> boundaries; };
int main() {
    const std::vector<Fixture> fixtures = {
        {u"", {0}}, {u"A😀中", {0, 1, 3, 4}}, {u"e\u0301", {0, 2}},
        {u"👩‍💻", {0, 5}}, {u"👍🏽", {0, 4}}, {u"🇨🇳", {0, 4}},
        {u"\r\n", {0, 2}}, {u"각", {0, 3}}
    };
    for (const auto &fixture : fixtures) {
        const auto size = static_cast<int32_t>(fixture.text.size());
        for (int32_t cursor = 0; cursor <= size; ++cursor) {
            const auto actual = FrdImeGraphemeNeighbors(fixture.text.data(), size, cursor);
            bool scalar = !(cursor > 0 && cursor < size && fixture.text[cursor-1] >= 0xD800 &&
                fixture.text[cursor-1] <= 0xDBFF && fixture.text[cursor] >= 0xDC00 && fixture.text[cursor] <= 0xDFFF);
            if (!scalar) { if (actual.valid) return 1; continue; }
            int32_t previous = 0, next = size; bool boundary = false;
            for (const auto b : fixture.boundaries) {
                if (b < cursor) previous = b;
                if (b == cursor) boundary = true;
                if (b > cursor) { next = b; break; }
            }
            if (!actual.valid || actual.is_boundary != boundary || actual.previous != previous || actual.next != next) return 2;
        }
    }
    const char16_t high[] = {0xD800}, low[] = {0xDC00};
    const std::u16string oversized(4097, u'a');
    if (FrdImeGraphemeNeighbors(high, 1, 0).valid || FrdImeGraphemeNeighbors(low, 1, 1).valid ||
        FrdImeGraphemeNeighbors(nullptr, 1, 0).valid || FrdImeGraphemeNeighbors(u"a", 1, -1).valid ||
        FrdImeGraphemeNeighbors(u"a", 1, 2).valid || FrdImeGraphemeNeighbors(u"a", -1, 0).valid ||
        FrdImeGraphemeNeighbors(oversized.data(), 4097, 0).valid) return 3;
    std::puts("PASS public ICU synthetic contract"); return 0;
}
