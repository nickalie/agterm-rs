// Headless probe of the WPE side alone (no GPUI): loads a URL, acknowledges every frame at once, and prints what the
// frames are (dmabuf fourcc/modifier/planes or SHM format) plus the frame rate. Usage: probe DEVICE|- URL SECONDS
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <glib.h>

typedef struct {
    uint32_t kind, width, height, fourcc;
    uint64_t modifier;
    uint32_t n_planes;
    int32_t fds[4];
    uint32_t offsets[4], strides[4];
    const uint8_t *shm_data;
    size_t shm_len;
    uint32_t shm_stride, shm_format;
    const int32_t *damage;
    uint32_t n_damage;
    void *buffer;
    int64_t fence_wait_us;
} SpikeFrame;
typedef struct {
    const char *device, *url;
    int width, height;
    void (*on_frame)(void *, const SpikeFrame *);
    void (*on_event)(void *, int, const char *);
    void *ud;
} SpikeConfig;
int spike_run(const SpikeConfig *);
void spike_frame_done(void *);
void spike_buffer_release(void *);
void spike_quit(void);

static int frames;
static int64_t first_us;

static void on_frame(void *ud, const SpikeFrame *f)
{
    (void)ud;
    if (!frames++) {
        first_us = g_get_monotonic_time();
        if (f->kind == 1)
            printf("first frame: dmabuf %ux%u fourcc %.4s modifier 0x%016llx planes %u stride %u fence_wait_us %lld\n",
                f->width, f->height, (const char *)&f->fourcc, (unsigned long long)f->modifier, f->n_planes,
                f->strides[0], (long long)f->fence_wait_us);
        else
            printf("first frame: shm %ux%u format %u stride %u damage rects %u\n", f->width, f->height, f->shm_format,
                f->shm_stride, f->n_damage);
        fflush(stdout);
    }
    spike_frame_done(f->buffer);
    if (f->kind == 1)
        spike_buffer_release(f->buffer);
}

static void on_event(void *ud, int kind, const char *text)
{
    (void)ud;
    printf("event %d: %s\n", kind, text ? text : "");
    fflush(stdout);
}

static gpointer quitter(gpointer data)
{
    g_usleep((gulong)(GPOINTER_TO_INT(data)) * G_USEC_PER_SEC);
    double secs = (g_get_monotonic_time() - first_us) / 1e6;
    printf("frames %d in %.1fs since the first: %.1f fps\n", frames, secs, frames > 1 ? (frames - 1) / secs : 0.0);
    fflush(stdout);
    spike_quit();
    return NULL;
}

int main(int argc, char **argv)
{
    if (argc < 4) {
        fprintf(stderr, "usage: probe DEVICE|- URL SECONDS\n");
        return 2;
    }
    SpikeConfig config = {strcmp(argv[1], "-") ? argv[1] : NULL, argv[2], 1280, 720, on_frame, on_event, NULL};
    g_thread_new("quitter", quitter, GINT_TO_POINTER(atoi(argv[3])));
    return spike_run(&config);
}
