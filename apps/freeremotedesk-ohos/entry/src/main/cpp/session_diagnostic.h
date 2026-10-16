#ifndef FRD_SESSION_DIAGNOSTIC_H
#define FRD_SESSION_DIAGNOSTIC_H
#include <cstdint>
namespace FrdSessionDiagnostic {
// 纯状态只接收 app handle 和 ABI 整数；不能接收 snapshot 或文本。
enum class FallbackTransition { None, Entered, CodeChanged, Exited };
enum class Event { Initialize, Shutdown, Cleanup, FallbackEntered, FallbackCodeChanged, FallbackExited };
struct FallbackState {
    FallbackTransition Observe(uint64_t app, int32_t code) {
        if (app != application) { application = app; failed = false; lastCode = 0; }
        if (code == 0) {
            if (!failed) return FallbackTransition::None;
            failed = false; lastCode = 0;
            return FallbackTransition::Exited;
        }
        if (!failed) {
            failed = true; lastCode = code;
            return FallbackTransition::Entered;
        }
        if (lastCode != code) {
            lastCode = code;
            return FallbackTransition::CodeChanged;
        }
        return FallbackTransition::None;
    }
private:
    uint64_t application = 0;
    int32_t lastCode = 0;
    bool failed = false;
};
}
#endif
