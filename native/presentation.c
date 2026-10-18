/* Optional Wayland presentation feedback. GLFW owns the display and surface.
 * Our proxies inherit a private event queue. Main dispatches only pending
 * events after GLFW reads the display; rendering requests feedback before
 * swap/commit. No display read, blocking dispatch or GPU wait happens here.
 * One mutex serializes proxy/listener creation, pending dispatch and metrics;
 * callbacks run inside pending dispatch and must not reacquire that mutex.
 */
#define _POSIX_C_SOURCE 200809L
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <pthread.h>
#include <wayland-client.h>
#include "presentation-time-client-protocol.h"

#define EVENT_CAP 2048
#define PENDING_CAP 256
struct PresentationEvent {
    uint64_t frame_id, submit_ns, decode_submit_ns, gpu_ready_ns;
    uint64_t presented_ns, refresh_ns, sequence, flags, discarded;
};
struct Presentation;
struct FrameTick {
    struct Presentation *owner;
    struct wl_callback *proxy;
    struct FrameTick *next;
};
struct Feedback {
    struct Presentation *owner;
    struct wp_presentation_feedback *proxy;
    struct Feedback *next;
    struct PresentationEvent event;
};
struct Presentation {
    struct wl_display *display;
    struct wl_surface *surface;
    struct wl_surface *surface_wrapper;
    struct wl_event_queue *queue;
    struct wl_registry *registry;
    struct wp_presentation *proxy;
    pthread_mutex_t mutex;
    struct Feedback *pending;
    struct FrameTick *ticks;
    size_t tick_count;
    size_t pending_count, head, count;
    uint64_t dropped;
    int clock_id;
    struct PresentationEvent events[EVENT_CAP];
};
static uint64_t monotonic_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + ts.tv_nsec;
}
static void complete(struct Feedback *feedback) {
    struct Presentation *p = feedback->owner;
    struct Feedback **entry = &p->pending;
    while (*entry && *entry != feedback) entry = &(*entry)->next;
    if (*entry) { *entry = feedback->next; p->pending_count--; }
    if (p->count < EVENT_CAP) {
        p->events[(p->head + p->count) % EVENT_CAP] = feedback->event;
        p->count++;
    } else p->dropped++;
    wp_presentation_feedback_destroy(feedback->proxy);
    free(feedback);
}
static void sync_output(void *data, struct wp_presentation_feedback *proxy,
                        struct wl_output *output) {
    (void)data; (void)proxy; (void)output;
}
static void presented(void *data, struct wp_presentation_feedback *proxy,
                      uint32_t hi, uint32_t lo, uint32_t nsec, uint32_t refresh,
                      uint32_t seq_hi, uint32_t seq_lo, uint32_t flags) {
    (void)proxy;
    struct Feedback *f = data;
    f->event.presented_ns = (((uint64_t)hi << 32) | lo) * 1000000000ull + nsec;
    f->event.refresh_ns = refresh;
    f->event.sequence = ((uint64_t)seq_hi << 32) | seq_lo;
    f->event.flags = flags;
    complete(f);
}
static void discarded(void *data, struct wp_presentation_feedback *proxy) {
    (void)proxy;
    struct Feedback *f = data;
    f->event.discarded = 1;
    complete(f);
}
static const struct wp_presentation_feedback_listener feedback_listener = {
    .sync_output=sync_output, .presented=presented, .discarded=discarded,
};
static void frame_done(void *data, struct wl_callback *callback, uint32_t time) {
    (void)time; (void)callback;
    struct FrameTick *tick=data;
    struct Presentation *p=tick->owner;
    struct FrameTick **entry=&p->ticks;
    while (*entry && *entry!=tick) entry=&(*entry)->next;
    if (*entry) { *entry=tick->next; p->tick_count--; }
    wl_callback_destroy(tick->proxy);
    free(tick);
}
static const struct wl_callback_listener frame_listener = {.done=frame_done};
static void clock_id(void *data, struct wp_presentation *proxy, uint32_t clock) {
    (void)proxy; ((struct Presentation *)data)->clock_id = (int)clock;
}
static const struct wp_presentation_listener presentation_listener = {.clock_id=clock_id};
static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version) {
    (void)version;
    struct Presentation *p = data;
    if (!p->proxy && strcmp(interface, "wp_presentation") == 0) {
        p->proxy = wl_registry_bind(registry, name, &wp_presentation_interface, 1);
        wp_presentation_add_listener(p->proxy, &presentation_listener, p);
    }
}
static void global_remove(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data; (void)registry; (void)name;
}
static const struct wl_registry_listener registry_listener = {global, global_remove};

void bridge_presentation_free(void *ptr) {
    struct Presentation *p = ptr;
    if (!p) return;
    /* Caller has stopped rendering and pending dispatch before destruction. */
    while (p->pending) {
        struct Feedback *f = p->pending;
        p->pending = f->next;
        wp_presentation_feedback_destroy(f->proxy);
        free(f);
    }
    while (p->ticks) {
        struct FrameTick *tick=p->ticks;
        p->ticks=tick->next;
        wl_callback_destroy(tick->proxy);
        free(tick);
    }
    if (p->surface_wrapper) wl_proxy_wrapper_destroy(p->surface_wrapper);
    if (p->proxy) wp_presentation_destroy(p->proxy);
    if (p->registry) wl_registry_destroy(p->registry);
    if (p->queue) wl_event_queue_destroy(p->queue);
    pthread_mutex_destroy(&p->mutex);
    free(p);
}
void *bridge_presentation_new(void *display, void *surface) {
    if (!display || !surface) return NULL;
    struct Presentation *p = calloc(1, sizeof(*p));
    if (!p) return NULL;
    p->display=display; p->surface=surface; p->clock_id=-1;
    if (pthread_mutex_init(&p->mutex, NULL) != 0) { free(p); return NULL; }
    p->queue = wl_display_create_queue(p->display);
    if (!p->queue) { bridge_presentation_free(p); return NULL; }
    p->surface_wrapper=wl_proxy_create_wrapper(p->surface);
    if (!p->surface_wrapper) { bridge_presentation_free(p); return NULL; }
    wl_proxy_set_queue((struct wl_proxy *)p->surface_wrapper, p->queue);
    struct wl_display *wrapper = wl_proxy_create_wrapper(p->display);
    if (!wrapper) { bridge_presentation_free(p); return NULL; }
    wl_proxy_set_queue((struct wl_proxy *)wrapper, p->queue);
    p->registry = wl_display_get_registry(wrapper);
    wl_proxy_wrapper_destroy(wrapper);
    if (!p->registry) { bridge_presentation_free(p); return NULL; }
    wl_registry_add_listener(p->registry, &registry_listener, p);
    /* Initial setup only, before the renderer starts using the connection. */
    if (wl_display_roundtrip_queue(p->display, p->queue) < 0 || !p->proxy ||
        wl_display_roundtrip_queue(p->display, p->queue) < 0 || p->clock_id < 0) {
        bridge_presentation_free(p); return NULL;
    }
    return p;
}
int bridge_presentation_request(void *ptr, uint64_t id, uint64_t decode_ns, uint64_t ready_ns) {
    struct Presentation *p = ptr;
    if (!p) return 0;
    pthread_mutex_lock(&p->mutex);
    if (p->pending_count >= PENDING_CAP || p->tick_count >= PENDING_CAP) {
        p->dropped++; pthread_mutex_unlock(&p->mutex); return 0;
    }
    struct Feedback *f = calloc(1, sizeof(*f));
    struct FrameTick *tick = calloc(1, sizeof(*tick));
    if (!f || !tick) { free(f); free(tick); p->dropped++; pthread_mutex_unlock(&p->mutex); return 0; }
    f->owner=p;
    f->event=(struct PresentationEvent){.frame_id=id, .submit_ns=monotonic_ns(),
        .decode_submit_ns=decode_ns, .gpu_ready_ns=ready_ns};
    f->proxy=wp_presentation_feedback(p->proxy, p->surface);
    if (!f->proxy) { free(f); free(tick); p->dropped++; pthread_mutex_unlock(&p->mutex); return 0; }
    tick->owner=p;
    tick->proxy=wl_surface_frame(p->surface_wrapper);
    if (!tick->proxy) {
        wp_presentation_feedback_destroy(f->proxy); free(f); free(tick);
        p->dropped++; pthread_mutex_unlock(&p->mutex); return 0;
    }
    wl_callback_add_listener(tick->proxy, &frame_listener, tick);
    tick->next=p->ticks; p->ticks=tick; p->tick_count++;
    wp_presentation_feedback_add_listener(f->proxy, &feedback_listener, f);
    f->next=p->pending; p->pending=f; p->pending_count++;
    pthread_mutex_unlock(&p->mutex);
    return 1;
}
int bridge_presentation_pump(void *ptr) {
    struct Presentation *p = ptr;
    if (!p) return 0;
    pthread_mutex_lock(&p->mutex);
    int result=wl_display_dispatch_queue_pending(p->display, p->queue);
    pthread_mutex_unlock(&p->mutex);
    return result;
}
size_t bridge_presentation_read(void *ptr, struct PresentationEvent *events, size_t capacity) {
    struct Presentation *p = ptr;
    if (!p || !events) return 0;
    pthread_mutex_lock(&p->mutex);
    size_t count=p->count < capacity ? p->count : capacity;
    for (size_t i=0;i<count;i++) events[i]=p->events[(p->head+i)%EVENT_CAP];
    p->head=(p->head+count)%EVENT_CAP; p->count-=count;
    pthread_mutex_unlock(&p->mutex);
    return count;
}
int bridge_presentation_clock(void *ptr) {
    return ptr ? ((struct Presentation *)ptr)->clock_id : -1;
}
uint64_t bridge_presentation_dropped(void *ptr) {
    if (!ptr) return 0;
    struct Presentation *p=ptr;
    pthread_mutex_lock(&p->mutex);
    uint64_t value=p->dropped;
    pthread_mutex_unlock(&p->mutex);
    return value;
}
size_t bridge_presentation_frame_pending(void *ptr) {
    if (!ptr) return 0;
    struct Presentation *p=ptr;
    pthread_mutex_lock(&p->mutex);
    size_t value=p->tick_count;
    pthread_mutex_unlock(&p->mutex);
    return value;
}
