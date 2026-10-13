// 非导出 AES256-GCM 密钥仅存在 HUKS；调用不回调 NAPI，不记录输入或错误原文。
#include <CryptoArchitectureKit/crypto_rand.h>
#include <huks/native_huks_api.h>
#include <huks/native_huks_param.h>
#include <huks/native_huks_type.h>
#include <string.h>
static uint8_t aliasText[] = "org.freeremotedesk.credentials.aes256.v1";
static struct OH_Huks_Blob alias = {sizeof(aliasText) - 1, aliasText};
static int params(struct OH_Huks_ParamSet **set, struct OH_Huks_Param *items, uint32_t count) {
    if (OH_Huks_InitParamSet(set).errorCode != 0)
        return -1;
    if (OH_Huks_AddParams(*set, items, count).errorCode != 0 ||
        OH_Huks_BuildParamSet(set).errorCode != 0) {
        OH_Huks_FreeParamSet(set);
        return -1;
    }
    return 0;
}
static int key(void) {
    struct OH_Huks_Result result = OH_Huks_IsKeyItemExist(&alias, NULL);
    if (result.errorCode == 0)
        return 0;
    if (result.errorCode != OH_HUKS_ERR_CODE_ITEM_NOT_EXIST)
        return -1;
    struct OH_Huks_Param items[] = {
        {.tag = OH_HUKS_TAG_ALGORITHM, .uint32Param = OH_HUKS_ALG_AES},
        {.tag = OH_HUKS_TAG_PURPOSE,
         .uint32Param = OH_HUKS_KEY_PURPOSE_ENCRYPT | OH_HUKS_KEY_PURPOSE_DECRYPT},
        {.tag = OH_HUKS_TAG_KEY_SIZE, .uint32Param = OH_HUKS_AES_KEY_SIZE_256},
        {.tag = OH_HUKS_TAG_PADDING, .uint32Param = OH_HUKS_PADDING_NONE},
        {.tag = OH_HUKS_TAG_BLOCK_MODE, .uint32Param = OH_HUKS_MODE_GCM},
        {.tag = OH_HUKS_TAG_KEY_STORAGE_FLAG, .uint32Param = OH_HUKS_STORAGE_PERSISTENT}};
    struct OH_Huks_ParamSet *set = NULL;
    if (params(&set, items, sizeof(items) / sizeof(items[0])) != 0)
        return -1;
    result = OH_Huks_GenerateKeyItem(&alias, set, NULL);
    OH_Huks_FreeParamSet(&set);
    return result.errorCode == 0 ? 0 : -1;
}
int32_t frd_huks_random(uint8_t *out) {
    OH_CryptoRand *ctx = NULL;
    Crypto_DataBlob blob = {0};
    if (OH_CryptoRand_Create(&ctx) != CRYPTO_SUCCESS)
        return -1;
    int ok = OH_CryptoRand_GenerateRandom(ctx, 12, &blob) == CRYPTO_SUCCESS && blob.len == 12;
    if (ok)
        memcpy(out, blob.data, 12);
    OH_Crypto_FreeDataBlob(&blob);
    OH_CryptoRand_Destroy(ctx);
    return ok ? 0 : -1;
}
int32_t frd_huks_crypt(uint32_t decrypt, const uint8_t *nonce, const uint8_t *aad, uint32_t aadLen,
                       const uint8_t *input, uint32_t inputLen, uint8_t *output,
                       uint32_t *outputLen) {
    if (decrypt && inputLen < 16)
        return -1;
    // 读取时不生成新 key：丢失密钥是 unavailable，不能覆盖既存 ciphertext 的 identity。
    if ((!decrypt && key() != 0) ||
        (decrypt && OH_Huks_IsKeyItemExist(&alias, NULL).errorCode != 0))
        return -1;
    struct OH_Huks_Param items[] = {
        {.tag = OH_HUKS_TAG_ALGORITHM, .uint32Param = OH_HUKS_ALG_AES},
        {.tag = OH_HUKS_TAG_PURPOSE,
         .uint32Param = decrypt ? OH_HUKS_KEY_PURPOSE_DECRYPT : OH_HUKS_KEY_PURPOSE_ENCRYPT},
        {.tag = OH_HUKS_TAG_KEY_SIZE, .uint32Param = OH_HUKS_AES_KEY_SIZE_256},
        {.tag = OH_HUKS_TAG_PADDING, .uint32Param = OH_HUKS_PADDING_NONE},
        {.tag = OH_HUKS_TAG_BLOCK_MODE, .uint32Param = OH_HUKS_MODE_GCM},
        {.tag = OH_HUKS_TAG_NONCE, .blob = {12, (uint8_t *)nonce}},
        {.tag = OH_HUKS_TAG_ASSOCIATED_DATA, .blob = {aadLen, (uint8_t *)aad}},
        {.tag = OH_HUKS_TAG_AE_TAG,
         .blob = {16, decrypt ? (uint8_t *)input + inputLen - 16 : NULL}}};
    struct OH_Huks_ParamSet *set = NULL;
    if (params(&set, items, decrypt ? 8 : 7) != 0)
        return -1;
    uint64_t handleValue = 0;
    struct OH_Huks_Blob handle = {sizeof(handleValue), (uint8_t *)&handleValue};
    struct OH_Huks_Blob in = {decrypt ? inputLen - 16 : inputLen, (uint8_t *)input};
    struct OH_Huks_Blob out = {*outputLen, output};
    struct OH_Huks_Result result = OH_Huks_InitSession(&alias, set, &handle, NULL);
    if (result.errorCode == 0) {
        result = OH_Huks_FinishSession(&handle, set, &in, &out);
        if (result.errorCode != 0)
            OH_Huks_AbortSession(&handle, set);
    }
    OH_Huks_FreeParamSet(&set);
    *outputLen = out.size;
    return result.errorCode == 0 ? 0 : -1;
}
