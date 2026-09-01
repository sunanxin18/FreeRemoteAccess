#include <limits.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include <libavcodec/avcodec.h>
#include <libavutil/avutil.h>
#include <libavutil/frame.h>
#include <libavutil/mem.h>
#include <libavutil/pixfmt.h>

enum {
    FRD_NATIVE_OK = 0,
    FRD_NATIVE_AGAIN = 1,
    FRD_NATIVE_EOF = 2,
    FRD_NATIVE_UNSUPPORTED = -1,
    FRD_NATIVE_INVALID_ARGUMENT = -2,
    FRD_NATIVE_DECODE_FAILED = -3,
};

typedef struct FrdNativeDecoder {
    AVCodecContext *codec;
    AVFrame *frame;
} FrdNativeDecoder;

typedef struct FrdNativeFrameView {
    int32_t format;
    int32_t width;
    int32_t height;
    int64_t timestamp_ticks;
    const uint8_t *data[3];
    int32_t linesize[3];
} FrdNativeFrameView;

static int32_t map_status(int result) {
    if (result >= 0) {
        return FRD_NATIVE_OK;
    }
    if (result == AVERROR(EAGAIN)) {
        return FRD_NATIVE_AGAIN;
    }
    if (result == AVERROR_EOF) {
        return FRD_NATIVE_EOF;
    }
    return FRD_NATIVE_DECODE_FAILED;
}

uint32_t frd_native_avcodec_major(void) {
    return avcodec_version() >> 16;
}

int32_t frd_native_hevc_decoder_available(void) {
    return avcodec_find_decoder(AV_CODEC_ID_HEVC) != NULL;
}

int32_t frd_native_yuv444p_format(void) {
    return AV_PIX_FMT_YUV444P;
}

int32_t frd_native_decoder_create(const uint8_t *extradata,
                                  size_t extradata_len,
                                  int32_t width,
                                  int32_t height,
                                  uint32_t timebase,
                                  FrdNativeDecoder **output) {
    const AVCodec *decoder;
    FrdNativeDecoder *state;
    int result;

    if (output == NULL) {
        return FRD_NATIVE_INVALID_ARGUMENT;
    }
    *output = NULL;
    if (extradata == NULL || extradata_len == 0 || extradata_len > INT_MAX ||
        width <= 0 || height <= 0 || timebase == 0 || timebase > (uint32_t)INT_MAX) {
        return FRD_NATIVE_INVALID_ARGUMENT;
    }

    decoder = avcodec_find_decoder(AV_CODEC_ID_HEVC);
    if (decoder == NULL) {
        return FRD_NATIVE_UNSUPPORTED;
    }
    state = (FrdNativeDecoder *)calloc(1, sizeof(*state));
    if (state == NULL) {
        return FRD_NATIVE_DECODE_FAILED;
    }
    state->codec = avcodec_alloc_context3(decoder);
    state->frame = av_frame_alloc();
    if (state->codec == NULL || state->frame == NULL) {
        av_frame_free(&state->frame);
        avcodec_free_context(&state->codec);
        free(state);
        return FRD_NATIVE_DECODE_FAILED;
    }

    state->codec->codec_type = AVMEDIA_TYPE_VIDEO;
    state->codec->codec_id = AV_CODEC_ID_HEVC;
    state->codec->width = width;
    state->codec->height = height;
    state->codec->pkt_timebase.num = 1;
    state->codec->pkt_timebase.den = (int)timebase;
    state->codec->extradata = (uint8_t *)av_mallocz(extradata_len + AV_INPUT_BUFFER_PADDING_SIZE);
    if (state->codec->extradata == NULL) {
        av_frame_free(&state->frame);
        avcodec_free_context(&state->codec);
        free(state);
        return FRD_NATIVE_DECODE_FAILED;
    }
    memcpy(state->codec->extradata, extradata, extradata_len);
    state->codec->extradata_size = (int)extradata_len;

    result = avcodec_open2(state->codec, decoder, NULL);
    if (result < 0) {
        av_frame_free(&state->frame);
        avcodec_free_context(&state->codec);
        free(state);
        return FRD_NATIVE_DECODE_FAILED;
    }
    *output = state;
    return FRD_NATIVE_OK;
}

int32_t frd_native_decoder_submit(FrdNativeDecoder *state,
                                  const uint8_t *data,
                                  size_t len,
                                  int64_t timestamp_ticks,
                                  int32_t random_access) {
    AVPacket *packet;
    int result;

    if (state == NULL || state->codec == NULL || data == NULL || len == 0 || len > INT_MAX) {
        return FRD_NATIVE_INVALID_ARGUMENT;
    }
    packet = av_packet_alloc();
    if (packet == NULL) {
        return FRD_NATIVE_DECODE_FAILED;
    }
    result = av_new_packet(packet, (int)len);
    if (result < 0) {
        av_packet_free(&packet);
        return FRD_NATIVE_DECODE_FAILED;
    }
    memcpy(packet->data, data, len);
    packet->pts = timestamp_ticks;
    packet->dts = timestamp_ticks;
    if (random_access != 0) {
        packet->flags |= AV_PKT_FLAG_KEY;
    }
    result = avcodec_send_packet(state->codec, packet);
    av_packet_free(&packet);
    return map_status(result);
}

int32_t frd_native_decoder_receive(FrdNativeDecoder *state, FrdNativeFrameView *output) {
    int result;
    int index;

    if (state == NULL || state->codec == NULL || state->frame == NULL || output == NULL) {
        return FRD_NATIVE_INVALID_ARGUMENT;
    }
    memset(output, 0, sizeof(*output));
    av_frame_unref(state->frame);
    result = avcodec_receive_frame(state->codec, state->frame);
    if (result < 0) {
        return map_status(result);
    }
    output->format = state->frame->format;
    output->width = state->frame->width;
    output->height = state->frame->height;
    output->timestamp_ticks = state->frame->pts;
    for (index = 0; index < 3; ++index) {
        output->data[index] = state->frame->data[index];
        output->linesize[index] = state->frame->linesize[index];
    }
    return FRD_NATIVE_OK;
}

int32_t frd_native_decoder_flush(FrdNativeDecoder *state) {
    if (state == NULL || state->codec == NULL) {
        return FRD_NATIVE_INVALID_ARGUMENT;
    }
    return map_status(avcodec_send_packet(state->codec, NULL));
}

void frd_native_decoder_destroy(FrdNativeDecoder *state) {
    if (state == NULL) {
        return;
    }
    av_frame_free(&state->frame);
    avcodec_free_context(&state->codec);
    free(state);
}
