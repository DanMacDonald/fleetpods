#include <libavcodec/avcodec.h>
#include <libavutil/channel_layout.h>
#include <stdlib.h>
#include <string.h>

struct Eld {
    AVCodecContext *context;
    AVPacket *packet;
    AVFrame *frame;
};

void eld_close(void *handle);

void *eld_open(const uint8_t *config, int config_len, int rate)
{
    const AVCodec *codec = avcodec_find_decoder(AV_CODEC_ID_AAC);
    AVCodecContext *context;
    struct Eld *eld;
    if (!codec || config_len <= 0 || rate <= 0) return NULL;
    context = avcodec_alloc_context3(codec);
    if (!context) return NULL;
    context->extradata = av_mallocz((size_t)config_len + AV_INPUT_BUFFER_PADDING_SIZE);
    if (!context->extradata) {
        avcodec_free_context(&context);
        return NULL;
    }
    memcpy(context->extradata, config, (size_t)config_len);
    context->extradata_size = config_len;
    context->sample_rate = rate;
    av_channel_layout_default(&context->ch_layout, 1);
    if (avcodec_open2(context, codec, NULL) < 0) {
        avcodec_free_context(&context);
        return NULL;
    }
    eld = calloc(1, sizeof *eld);
    if (!eld) {
        avcodec_free_context(&context);
        return NULL;
    }
    eld->context = context;
    eld->packet = av_packet_alloc();
    eld->frame = av_frame_alloc();
    if (!eld->packet || !eld->frame) {
        eld_close(eld);
        return NULL;
    }
    return eld;
}

int eld_decode(void *handle, const uint8_t *data, int len, float *out, int out_cap,
    int expect_samples, int expect_rate)
{
    struct Eld *eld = handle;
    int result, count;
    if (!eld || !data || len <= 0 || !out) return -1;
    av_packet_unref(eld->packet);
    if (av_new_packet(eld->packet, len) < 0) return -1;
    memcpy(eld->packet->data, data, (size_t)len);
    result = avcodec_send_packet(eld->context, eld->packet);
    if (result >= 0) result = avcodec_receive_frame(eld->context, eld->frame);
    if (result < 0) return -1;
    count = eld->frame->nb_samples;
    if (eld->frame->sample_rate != expect_rate || eld->frame->ch_layout.nb_channels != 1
        || count != expect_samples || eld->frame->format != AV_SAMPLE_FMT_FLTP
        || count > out_cap || !eld->frame->data[0]) {
        av_frame_unref(eld->frame);
        return -1;
    }
    memcpy(out, eld->frame->data[0], (size_t)count * sizeof(float));
    av_frame_unref(eld->frame);
    return count;
}

void eld_close(void *handle)
{
    struct Eld *eld = handle;
    if (!eld) return;
    av_frame_free(&eld->frame);
    av_packet_free(&eld->packet);
    avcodec_free_context(&eld->context);
    free(eld);
}
