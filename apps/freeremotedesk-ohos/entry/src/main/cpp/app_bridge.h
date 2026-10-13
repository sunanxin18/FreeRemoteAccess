#pragma once
#include "napi/native_api.h"
void RegisterFrdApp(napi_env env, napi_value exports);
void FrdAppForeground(bool value);
void FrdAppCleanup();

#include <cstdint>
int32_t FrdAppBindRenderer(uint64_t renderer);
void FrdAppRemoteFocus(bool foreground, bool focused);
int32_t FrdAppNativePointer(uint64_t local, float x, float y, uint32_t buttons, bool takeFocus, bool logical);
