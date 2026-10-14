// WPE WebKit host for the spike: a WPEPlatform headless display on its own thread and GMainContext, frames handed
// to Rust as dmabuf planes or SHM pixels, input posted from the GPUI thread. Throwaway code.
//
// The headless view's render_buffer/buffers_changed vfuncs are replaced at class level so the frames reach us; the
// host decides when a buffer counts as rendered (after GPUI presented it) and released (after a newer one is shown).

#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <wpe/webkit.h>
#include <wpe/wpe-platform.h>
#include <wpe/headless/wpe-headless.h>

typedef struct {
    uint32_t kind; // 1 dmabuf, 2 shm
    uint32_t width, height;
    uint32_t fourcc;
    uint64_t modifier;
    uint32_t n_planes;
    int32_t fds[4];
    uint32_t offsets[4];
    uint32_t strides[4];
    const uint8_t *shm_data;
    size_t shm_len;
    uint32_t shm_stride;
    uint32_t shm_format;
    const int32_t *damage; // x, y, w, h per rect
    uint32_t n_damage;
    void *buffer; // WPEBuffer*, the frame's id for spike_frame_done/spike_buffer_release
    int64_t fence_wait_us;
} SpikeFrame;

// kind: 1 frame callback (frame), 2 load finished, 3 js result (text), 4 buffers changed, 5 error (text)
typedef void (*SpikeFrameCb)(void *ud, const SpikeFrame *frame);
typedef void (*SpikeEventCb)(void *ud, int kind, const char *text);

typedef struct {
    const char *device; // render node or NULL for the headless default
    const char *url;
    int width, height;
    SpikeFrameCb on_frame;
    SpikeEventCb on_event;
    void *ud;
} SpikeConfig;

typedef struct {
    GMainContext *ctx;
    GMainLoop *loop;
    WPEDisplay *display;
    WebKitWebView *web_view;
    WPEView *view;
    SpikeFrameCb on_frame;
    SpikeEventCb on_event;
    void *ud;
    GPtrArray *buffers; // the current WebKit buffer set (not owned): acknowledgements for others are dropped
} SpikeHost;

static SpikeHost *g_host;
static gboolean (*orig_render_buffer)(WPEView *, WPEBuffer *, const WPERectangle *, guint, GError **);
static void (*orig_buffers_changed)(WPEView *, WPEBuffer **, guint);

static int64_t now_us(void) { return g_get_monotonic_time(); }

static int64_t wait_rendering_fence(WPEBuffer *buffer)
{
    int fd = wpe_buffer_get_rendering_fence(buffer);
    if (fd < 0)
        return -1;
    int64_t start = now_us();
    struct pollfd pfd = {.fd = fd, .events = POLLIN};
    while (poll(&pfd, 1, 100) < 0 && errno == EINTR) { }
    return now_us() - start;
}

static gboolean hook_render_buffer(WPEView *view, WPEBuffer *buffer, const WPERectangle *damage, guint n_damage,
    GError **error)
{
    (void)error;
    SpikeHost *host = g_host;
    if (!host || view != host->view)
        return orig_render_buffer ? orig_render_buffer(view, buffer, damage, n_damage, error) : TRUE;

    SpikeFrame frame;
    memset(&frame, 0, sizeof frame);
    frame.width = (uint32_t)wpe_buffer_get_width(buffer);
    frame.height = (uint32_t)wpe_buffer_get_height(buffer);
    frame.buffer = buffer;
    frame.damage = (const int32_t *)damage; // WPERectangle is four ints
    frame.n_damage = n_damage;
    frame.fence_wait_us = wait_rendering_fence(buffer);

    if (WPE_IS_BUFFER_DMA_BUF(buffer)) {
        WPEBufferDMABuf *dmabuf = WPE_BUFFER_DMA_BUF(buffer);
        frame.kind = 1;
        frame.fourcc = wpe_buffer_dma_buf_get_format(dmabuf);
        frame.modifier = wpe_buffer_dma_buf_get_modifier(dmabuf);
        frame.n_planes = wpe_buffer_dma_buf_get_n_planes(dmabuf);
        for (guint i = 0; i < frame.n_planes && i < 4; i++) {
            frame.fds[i] = wpe_buffer_dma_buf_get_fd(dmabuf, i);
            frame.offsets[i] = wpe_buffer_dma_buf_get_offset(dmabuf, i);
            frame.strides[i] = wpe_buffer_dma_buf_get_stride(dmabuf, i);
        }
        host->on_frame(host->ud, &frame);
    } else if (WPE_IS_BUFFER_SHM(buffer)) {
        WPEBufferSHM *shm = WPE_BUFFER_SHM(buffer);
        GBytes *bytes = wpe_buffer_shm_get_data(shm);
        gsize len = 0;
        frame.kind = 2;
        frame.shm_data = g_bytes_get_data(bytes, &len);
        frame.shm_len = len;
        frame.shm_stride = wpe_buffer_shm_get_stride(shm);
        frame.shm_format = (uint32_t)wpe_buffer_shm_get_format(shm);
        // Rust copies the damaged rects out inside the callback, so the buffer goes back to WebKit right away.
        host->on_frame(host->ud, &frame);
        wpe_view_buffer_released(view, buffer);
    } else {
        host->on_event(host->ud, 5, "unknown WPEBuffer type");
        wpe_view_buffer_rendered(view, buffer);
        wpe_view_buffer_released(view, buffer);
    }
    return TRUE;
}

static void hook_buffers_changed(WPEView *view, WPEBuffer **buffers, guint n_buffers)
{
    if (orig_buffers_changed)
        orig_buffers_changed(view, buffers, n_buffers);
    SpikeHost *host = g_host;
    if (!host || view != host->view)
        return;
    // The set grows while WebKit waits for releases; only a removed buffer invalidates imports keyed by pointer.
    guint removed = 0;
    for (guint i = 0; i < host->buffers->len; i++) {
        gboolean kept = FALSE;
        for (guint j = 0; j < n_buffers && !kept; j++)
            kept = buffers[j] == g_ptr_array_index(host->buffers, i);
        removed += !kept;
    }
    g_ptr_array_set_size(host->buffers, 0);
    for (guint j = 0; j < n_buffers; j++)
        g_ptr_array_add(host->buffers, buffers[j]);
    char text[64];
    snprintf(text, sizeof text, "%u buffers, %u removed", n_buffers, removed);
    host->on_event(host->ud, removed ? 4 : 6, text);
}

static void on_load_changed(WebKitWebView *web_view, WebKitLoadEvent event, gpointer data)
{
    SpikeHost *host = data;
    if (event == WEBKIT_LOAD_FINISHED)
        host->on_event(host->ud, 2, webkit_web_view_get_uri(web_view));
}

static gboolean on_load_failed(WebKitWebView *web_view, WebKitLoadEvent event, char *uri, GError *error, gpointer data)
{
    (void)web_view;
    (void)event;
    SpikeHost *host = data;
    char *text = g_strdup_printf("load failed %s: %s", uri, error->message);
    host->on_event(host->ud, 5, text);
    g_free(text);
    return FALSE;
}

// SHM fallback: a display with no DRM device and no dmabuf formats makes WebKit hand over WPEBufferSHM frames.
static WPEDRMDevice *no_drm_device(WPEDisplay *display)
{
    (void)display;
    return NULL;
}

static WPEBufferFormats *no_buffer_formats(WPEDisplay *display)
{
    (void)display;
    return NULL;
}

static void install_hooks(void)
{
    if (g_getenv("SPIKE_FORCE_SHM")) {
        WPEDisplayClass *display_class = g_type_class_ref(wpe_display_headless_get_type());
        display_class->get_drm_device = no_drm_device;
        display_class->get_preferred_buffer_formats = no_buffer_formats;
    }
    WPEViewClass *klass = g_type_class_ref(wpe_view_headless_get_type());
    if (klass->render_buffer != hook_render_buffer) {
        orig_render_buffer = klass->render_buffer;
        orig_buffers_changed = klass->buffers_changed;
        klass->render_buffer = hook_render_buffer;
        klass->buffers_changed = hook_buffers_changed;
    }
}

// Runs the host on the calling thread until spike_quit; returns 0 on a clean exit.
int spike_run(const SpikeConfig *config)
{
    SpikeHost *host = g_new0(SpikeHost, 1);
    host->on_frame = config->on_frame;
    host->on_event = config->on_event;
    host->ud = config->ud;
    host->ctx = g_main_context_new();
    host->buffers = g_ptr_array_new();
    g_main_context_push_thread_default(host->ctx);

    GError *error = NULL;
    install_hooks();
    if (config->device && *config->device)
        host->display = wpe_display_headless_new_for_device(config->device, &error);
    else
        host->display = wpe_display_headless_new();
    if (!host->display || !wpe_display_connect(host->display, &error)) {
        host->on_event(host->ud, 5, error ? error->message : "display failed");
        return 1;
    }
    wpe_display_set_primary(host->display);
    install_hooks();

    WebKitSettings *settings = webkit_settings_new();
    // SPIKE_REQUIRE_GESTURE=1 matches the GTK overlay's setting (html_overlay/page.rs).
    webkit_settings_set_media_playback_requires_user_gesture(settings, g_getenv("SPIKE_REQUIRE_GESTURE") != NULL);
    webkit_settings_set_enable_developer_extras(settings, TRUE);
    WebKitNetworkSession *session = webkit_network_session_new_ephemeral();
    host->web_view = WEBKIT_WEB_VIEW(g_object_new(WEBKIT_TYPE_WEB_VIEW, "display", host->display, "network-session",
        session, "settings", settings, NULL));
    host->view = webkit_web_view_get_wpe_view(host->web_view);
    g_host = host;

    WPEToplevel *toplevel = wpe_view_get_toplevel(host->view);
    if (toplevel)
        wpe_toplevel_resize(toplevel, config->width, config->height);
    else
        wpe_view_resized(host->view, config->width, config->height);
    wpe_view_map(host->view);
    wpe_view_set_visible(host->view, TRUE);
    wpe_view_focus_in(host->view);

    g_signal_connect(host->web_view, "load-changed", G_CALLBACK(on_load_changed), host);
    g_signal_connect(host->web_view, "load-failed", G_CALLBACK(on_load_failed), host);
    webkit_web_view_load_uri(host->web_view, config->url);

    host->loop = g_main_loop_new(host->ctx, FALSE);
    g_main_loop_run(host->loop);

    g_host = NULL;
    g_object_unref(host->web_view);
    g_object_unref(session);
    g_object_unref(settings);
    g_main_context_pop_thread_default(host->ctx);
    return 0;
}

// ---- calls from the GPUI thread: marshalled onto the WPE context ----

typedef struct {
    int op;
    void *buffer;
    int type, button, press_count;
    double x, y, dx, dy;
    uint32_t modifiers, keyval;
    int width, height;
    char *text;
} SpikeMsg;

enum { OP_RENDERED = 1, OP_RELEASE, OP_POINTER, OP_SCROLL, OP_KEY, OP_RESIZE, OP_EVAL, OP_LOAD, OP_QUIT };

static void msg_free(gpointer data)
{
    SpikeMsg *msg = data;
    g_free(msg->text);
    g_free(msg);
}

static void eval_done(GObject *object, GAsyncResult *result, gpointer data)
{
    (void)data;
    GError *error = NULL;
    JSCValue *value = webkit_web_view_evaluate_javascript_finish(WEBKIT_WEB_VIEW(object), result, &error);
    if (!g_host)
        return;
    if (!value) {
        g_host->on_event(g_host->ud, 3, error ? error->message : "eval failed");
        g_clear_error(&error);
        return;
    }
    char *text = jsc_value_to_string(value);
    g_host->on_event(g_host->ud, 3, text);
    g_free(text);
    g_object_unref(value);
}

static guint keycode_for(WPEView *view, guint keyval)
{
    WPEKeymap *keymap = wpe_display_get_keymap(wpe_view_get_display(view));
    WPEKeymapEntry *entries = NULL;
    guint n = 0;
    guint keycode = 0;
    if (keymap && wpe_keymap_get_entries_for_keyval(keymap, keyval, &entries, &n) && n > 0)
        keycode = entries[0].keycode;
    g_free(entries);
    return keycode;
}

static gboolean dispatch(gpointer data)
{
    SpikeMsg *msg = data;
    SpikeHost *host = g_host;
    if (!host)
        return G_SOURCE_REMOVE;
    WPEView *view = host->view;
    guint32 time = (guint32)(now_us() / 1000);
    WPEEvent *event = NULL;
    switch (msg->op) {
    case OP_RENDERED:
        if (g_ptr_array_find(host->buffers, msg->buffer, NULL))
            wpe_view_buffer_rendered(view, msg->buffer);
        break;
    case OP_RELEASE:
        if (g_ptr_array_find(host->buffers, msg->buffer, NULL))
            wpe_view_buffer_released(view, msg->buffer);
        break;
    case OP_POINTER:
        if (msg->type == WPE_EVENT_POINTER_MOVE)
            event = wpe_event_pointer_move_new(WPE_EVENT_POINTER_MOVE, view, WPE_INPUT_SOURCE_MOUSE, time,
                msg->modifiers, msg->x, msg->y, msg->dx, msg->dy);
        else
            event = wpe_event_pointer_button_new(msg->type, view, WPE_INPUT_SOURCE_MOUSE, time, msg->modifiers,
                (guint)msg->button, msg->x, msg->y, (guint)msg->press_count);
        break;
    case OP_SCROLL:
        event = wpe_event_scroll_new(view, WPE_INPUT_SOURCE_MOUSE, time, msg->modifiers, msg->dx, msg->dy,
            msg->press_count != 0, FALSE, msg->x, msg->y);
        break;
    case OP_KEY:
        event = wpe_event_keyboard_new(msg->type, view, WPE_INPUT_SOURCE_KEYBOARD, time, msg->modifiers,
            keycode_for(view, msg->keyval), msg->keyval);
        break;
    case OP_RESIZE: {
        WPEToplevel *toplevel = wpe_view_get_toplevel(view);
        if (toplevel)
            wpe_toplevel_resize(toplevel, msg->width, msg->height);
        else
            wpe_view_resized(view, msg->width, msg->height);
        break;
    }
    case OP_EVAL:
        webkit_web_view_evaluate_javascript(host->web_view, msg->text, -1, NULL, NULL, NULL, eval_done, NULL);
        break;
    case OP_LOAD:
        webkit_web_view_load_uri(host->web_view, msg->text);
        break;
    case OP_QUIT:
        g_main_loop_quit(host->loop);
        break;
    }
    if (event) {
        wpe_view_event(view, event);
        wpe_event_unref(event);
    }
    return G_SOURCE_REMOVE;
}

static void post(SpikeMsg *msg)
{
    SpikeHost *host = g_host;
    if (!host) {
        msg_free(msg);
        return;
    }
    // Always deferred (an idle source), even from the WPE thread itself: acknowledging a buffer from inside
    // render_buffer would re-enter WebKit before it recorded the frame as pending.
    GSource *source = g_idle_source_new();
    g_source_set_priority(source, G_PRIORITY_DEFAULT);
    g_source_set_callback(source, dispatch, msg, msg_free);
    g_source_attach(source, host->ctx);
    g_source_unref(source);
}

int spike_ready(void) { return g_host != NULL; }

void spike_frame_done(void *buffer)
{
    SpikeMsg *msg = g_new0(SpikeMsg, 1);
    msg->op = OP_RENDERED;
    msg->buffer = buffer;
    post(msg);
}

void spike_buffer_release(void *buffer)
{
    SpikeMsg *msg = g_new0(SpikeMsg, 1);
    msg->op = OP_RELEASE;
    msg->buffer = buffer;
    post(msg);
}

// type: 0 move, 1 down, 2 up
void spike_pointer(int type, double x, double y, int button, int press_count, uint32_t modifiers)
{
    static const int types[] = {WPE_EVENT_POINTER_MOVE, WPE_EVENT_POINTER_DOWN, WPE_EVENT_POINTER_UP};
    SpikeMsg *msg = g_new0(SpikeMsg, 1);
    msg->op = OP_POINTER;
    msg->type = types[type];
    msg->x = x;
    msg->y = y;
    msg->button = button;
    // WPE takes a press count on button-down events only.
    msg->press_count = type == 1 ? press_count : 0;
    msg->modifiers = modifiers;
    post(msg);
}

void spike_scroll(double x, double y, double dx, double dy, int precise, uint32_t modifiers)
{
    SpikeMsg *msg = g_new0(SpikeMsg, 1);
    msg->op = OP_SCROLL;
    msg->x = x;
    msg->y = y;
    msg->dx = dx;
    msg->dy = dy;
    msg->press_count = precise;
    msg->modifiers = modifiers;
    post(msg);
}

void spike_key(int down, uint32_t keyval, uint32_t modifiers)
{
    SpikeMsg *msg = g_new0(SpikeMsg, 1);
    msg->op = OP_KEY;
    msg->type = down ? WPE_EVENT_KEYBOARD_KEY_DOWN : WPE_EVENT_KEYBOARD_KEY_UP;
    msg->keyval = keyval;
    msg->modifiers = modifiers;
    post(msg);
}

void spike_resize(int width, int height)
{
    SpikeMsg *msg = g_new0(SpikeMsg, 1);
    msg->op = OP_RESIZE;
    msg->width = width;
    msg->height = height;
    post(msg);
}

static void post_text(int op, const char *text)
{
    SpikeMsg *msg = g_new0(SpikeMsg, 1);
    msg->op = op;
    msg->text = g_strdup(text);
    post(msg);
}

void spike_eval(const char *script) { post_text(OP_EVAL, script); }
void spike_load(const char *url) { post_text(OP_LOAD, url); }
void spike_quit(void) { post_text(OP_QUIT, NULL); }
