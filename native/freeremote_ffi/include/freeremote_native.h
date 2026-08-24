#ifndef FREEREMOTE_NATIVE_H
#define FREEREMOTE_NATIVE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define FRD_ABI_VERSION 1u

typedef struct FrdValidationOutput {
  uint32_t abi_version;
  uint32_t status;
  uint8_t protocol;
  uint8_t reserved;
  uint16_t port;
} FrdValidationOutput;

uint32_t frd_validate_connection(
    uint8_t service,
    const char *host,
    uint16_t port,
    const char *username,
    const char *password,
    const char *domain,
    FrdValidationOutput *output);

#ifdef __cplusplus
}
#endif

#endif
