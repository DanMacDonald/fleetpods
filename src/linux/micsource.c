#include <pipewire/pipewire.h>
#include <spa/param/audio/format-utils.h>
#include <spa/param/props.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

struct MicHooks {
    void (*fill)(void *, float *, int);
    void (*streaming)(void *, int);
    void (*muted)(void *, int);
    void (*format_ok)(void *, int);
};

struct MicRun {
    struct pw_stream *stream;
    void *user;
    struct MicHooks hooks;
    uint32_t rate;
    int broken;
    int closing;
};

static void report_streaming(struct MicRun *run, int on)
{
    if (run->hooks.streaming) run->hooks.streaming(run->user, on);
}

static void report_muted(struct MicRun *run, int on)
{
    if (run->hooks.muted) run->hooks.muted(run->user, on);
}

static void report_format(struct MicRun *run, int ok)
{
    if (run->hooks.format_ok) run->hooks.format_ok(run->user, ok);
}

static void on_state(void *data, enum pw_stream_state old, enum pw_stream_state state, const char *error)
{
    struct MicRun *run = data;
    if (run->closing) return;
    report_streaming(run, state == PW_STREAM_STATE_STREAMING);
    if (state == PW_STREAM_STATE_ERROR
        || (state == PW_STREAM_STATE_UNCONNECTED
            && (old == PW_STREAM_STATE_PAUSED || old == PW_STREAM_STATE_STREAMING))) {
        run->broken = 1;
        if (error && *error)
            fprintf(stderr, "fleetpodsd: microphone source failed: %s\n", error);
    }
}

static void on_control(void *data, uint32_t id, const struct pw_stream_control *control)
{
    struct MicRun *run = data;
    if (run->closing) return;
    if (id == SPA_PROP_mute && control && control->n_values && control->values)
        report_muted(run, control->values[0] != 0.0f);
}

static void on_format(void *data, uint32_t id, const struct spa_pod *param)
{
    struct MicRun *run = data;
    struct spa_audio_info_raw format;
    if (run->closing || id != SPA_PARAM_Format) return;
    if (!param) {
        report_format(run, 0);
        return;
    }
    memset(&format, 0, sizeof format);
    if (spa_format_audio_raw_parse(param, &format) < 0 || format.channels != 1
        || format.format != SPA_AUDIO_FORMAT_F32 || format.rate != run->rate) {
        report_format(run, 0);
        return;
    }
    report_format(run, 1);
}

static void on_process(void *data)
{
    struct MicRun *run = data;
    struct pw_buffer *buffer = pw_stream_dequeue_buffer(run->stream);
    struct spa_data *plane;
    float *samples;
    uint32_t capacity, count;
    if (!buffer || !buffer->buffer || !buffer->buffer->n_datas) return;
    plane = &buffer->buffer->datas[0];
    if (!plane->data || !plane->chunk) {
        pw_stream_queue_buffer(run->stream, buffer);
        return;
    }
    samples = (float *)plane->data;
    capacity = plane->maxsize / sizeof(float);
    count = buffer->requested && buffer->requested < capacity ? (uint32_t)buffer->requested : capacity;
    if (run->hooks.fill) run->hooks.fill(run->user, samples, (int)count);
    plane->chunk->offset = 0;
    plane->chunk->stride = sizeof(float);
    plane->chunk->size = count * sizeof(float);
    pw_stream_queue_buffer(run->stream, buffer);
}

static void pause_for_retry(const int *stop)
{
    int left = 5000;
    while (left > 0 && !__atomic_load_n(stop, __ATOMIC_ACQUIRE)) {
        struct timespec slice = {.tv_sec = 0, .tv_nsec = 200000000L};
        nanosleep(&slice, NULL);
        left -= 200;
    }
}

static void close_attempt(struct pw_stream *stream, struct pw_core *core, struct pw_context *context,
    struct pw_loop *loop)
{
    if (stream) pw_stream_destroy(stream);
    if (core) pw_core_disconnect(core);
    if (context) pw_context_destroy(context);
    if (loop) pw_loop_destroy(loop);
}

/* One PipeWire connection. Returns 0 when the caller should try again. */
static int run_once(const char *name, const char *description, const char *address, uint32_t rate,
    void *user, const struct MicHooks *hooks, int *stop)
{
    struct pw_loop *loop;
    struct pw_context *context;
    struct pw_core *core;
    struct pw_properties *props;
    struct pw_stream_events events;
    struct MicRun run;
    uint8_t pod_buffer[1024];
    struct spa_pod_builder builder;
    struct spa_audio_info_raw info;
    const struct spa_pod *params[1];
    loop = pw_loop_new(NULL);
    if (!loop) return 0;
    context = pw_context_new(loop, NULL, 0);
    core = context ? pw_context_connect(context, NULL, 0) : NULL;
    if (!description || !*description) description = "AirPods";
    props = pw_properties_new(
        PW_KEY_NODE_NAME, name,
        PW_KEY_NODE_DESCRIPTION, description,
        PW_KEY_NODE_NICK, description,
        PW_KEY_MEDIA_CLASS, "Audio/Source",
        PW_KEY_MEDIA_TYPE, "Audio",
        PW_KEY_MEDIA_CATEGORY, "Capture",
        PW_KEY_MEDIA_ROLE, "Communication",
        PW_KEY_NODE_VIRTUAL, "false",
        "node.want-driver", "true",
        PW_KEY_NODE_LATENCY, "1/100",
        "priority.session", "0",
        "api.bluez5.address", address,
        NULL);
    memset(&events, 0, sizeof events);
    events.version = PW_VERSION_STREAM_EVENTS;
    events.state_changed = on_state;
    events.control_info = on_control;
    events.param_changed = on_format;
    events.process = on_process;
    memset(&run, 0, sizeof run);
    run.user = user;
    run.hooks = *hooks;
    run.rate = rate;
    run.stream = pw_stream_new_simple(loop, name, props, &events, &run);
    if (!core || !run.stream) {
        fprintf(stderr, "fleetpodsd: microphone source did not connect\n");
        close_attempt(run.stream, core, context, loop);
        return 0;
    }
    memset(&info, 0, sizeof info);
    info.format = SPA_AUDIO_FORMAT_F32;
    info.rate = rate;
    info.channels = 1;
    info.position[0] = SPA_AUDIO_CHANNEL_MONO;
    builder = SPA_POD_BUILDER_INIT(pod_buffer, sizeof pod_buffer);
    params[0] = spa_format_audio_raw_build(&builder, SPA_PARAM_EnumFormat, &info);
    if (pw_stream_connect(run.stream, PW_DIRECTION_OUTPUT, PW_ID_ANY,
            PW_STREAM_FLAG_MAP_BUFFERS | PW_STREAM_FLAG_RT_PROCESS, params, 1) < 0) {
        fprintf(stderr, "fleetpodsd: microphone source did not connect\n");
        close_attempt(run.stream, core, context, loop);
        return 0;
    }
    while (!__atomic_load_n(stop, __ATOMIC_ACQUIRE) && !run.broken)
        pw_loop_iterate(loop, 200);
    run.closing = 1;
    close_attempt(run.stream, core, context, loop);
    report_streaming(&run, 0);
    report_format(&run, 0);
    return __atomic_load_n(stop, __ATOMIC_ACQUIRE) ? 1 : 0;
}

void mic_source_run(const char *name, const char *description, const char *address, uint32_t rate,
    void *user, const struct MicHooks *hooks, int *stop)
{
    if (!name || !address || !rate || !user || !hooks || !hooks->fill || !stop) return;
    pw_init(NULL, NULL);
    while (!__atomic_load_n(stop, __ATOMIC_ACQUIRE)) {
        if (run_once(name, description, address, rate, user, hooks, stop)) break;
        if (__atomic_load_n(stop, __ATOMIC_ACQUIRE)) break;
        fprintf(stderr, "fleetpodsd: microphone source retrying\n");
        pause_for_retry(stop);
    }
    pw_deinit();
}
