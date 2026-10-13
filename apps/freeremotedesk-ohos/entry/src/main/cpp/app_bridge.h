#pragma once
#include "napi/native_api.h"
void RegisterFrdApp(napi_env env, napi_value exports);
void FrdAppForeground(bool value);
void FrdAppCleanup();
