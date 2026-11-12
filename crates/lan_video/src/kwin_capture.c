/*
 * KDE KWin screencast capture helper.
 *
 * 通过 zkde_screencast_unstable_v1 创建一路屏幕流（无需 portal 授权弹窗），
 * 把 PipeWire node id 打印到 stdout（NODE=<id>），然后保持 Wayland 连接存活。
 * 由 lan-video 的 SessionManager 以子进程方式拉起，杀掉子进程即结束采集。
 *
 * 权限：KWin 会按客户端可执行文件的 desktop 文件里
 * X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1 决定是否暴露该接口。
 */

#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <wayland-client.h>

#include "zkde-screencast-client-protocol.h"

static volatile sig_atomic_t g_running = 1;

static void on_signal(int sig)
{
    (void)sig;
    g_running = 0;
}

struct capture_state {
    struct wl_display *display;
    struct wl_registry *registry;
    struct zkde_screencast_unstable_v1 *screencast;
    struct wl_output *output;
    struct zkde_screencast_stream_unstable_v1 *stream;
    int got_node;
    int failed;
};

static void output_geometry(void *data, struct wl_output *output, int32_t x, int32_t y,
                            int32_t physical_width, int32_t physical_height, int32_t subpixel,
                            const char *make, const char *model, int32_t transform)
{
    (void)data; (void)output; (void)x; (void)y; (void)physical_width; (void)physical_height;
    (void)subpixel; (void)make; (void)model; (void)transform;
}

static void output_mode(void *data, struct wl_output *output, uint32_t flags, int32_t width,
                        int32_t height, int32_t refresh)
{
    (void)data; (void)output; (void)flags; (void)width; (void)height; (void)refresh;
}

static void output_done(void *data, struct wl_output *output)
{
    (void)data; (void)output;
}

static void output_scale(void *data, struct wl_output *output, int32_t factor)
{
    (void)data; (void)output; (void)factor;
}

static void output_name(void *data, struct wl_output *output, const char *name)
{
    (void)data; (void)output; (void)name;
}

static void output_description(void *data, struct wl_output *output, const char *description)
{
    (void)data; (void)output; (void)description;
}

static const struct wl_output_listener output_listener = {
    .geometry = output_geometry,
    .mode = output_mode,
    .done = output_done,
    .scale = output_scale,
    .name = output_name,
    .description = output_description,
};

static void stream_created(void *data, struct zkde_screencast_stream_unstable_v1 *stream,
                           uint32_t node)
{
    (void)stream;
    struct capture_state *state = data;
    state->got_node = 1;
    printf("NODE=%u\n", node);
    fflush(stdout);
}

static void stream_failed(void *data, struct zkde_screencast_stream_unstable_v1 *stream,
                          const char *error)
{
    (void)stream;
    struct capture_state *state = data;
    state->failed = 1;
    fprintf(stderr, "kwin-capture: failed: %s\n", error ? error : "unknown");
    fflush(stderr);
    g_running = 0;
}

static void stream_closed(void *data, struct zkde_screencast_stream_unstable_v1 *stream)
{
    (void)stream;
    (void)data;
    g_running = 0;
}

static void stream_serial(void *data, struct zkde_screencast_stream_unstable_v1 *stream,
                          uint32_t hi, uint32_t lo)
{
    (void)data; (void)stream; (void)hi; (void)lo;
}

static const struct zkde_screencast_stream_unstable_v1_listener stream_listener = {
    .created = stream_created,
    .failed = stream_failed,
    .closed = stream_closed,
    .serial = stream_serial,
};

static void registry_global(void *data, struct wl_registry *registry, uint32_t name,
                            const char *interface, uint32_t version)
{
    struct capture_state *state = data;
    if (strcmp(interface, wl_output_interface.name) == 0 && !state->output) {
        uint32_t bind_version = version < 4 ? version : 4;
        state->output = wl_registry_bind(registry, name, &wl_output_interface, bind_version);
        wl_output_add_listener(state->output, &output_listener, state);
    } else if (strcmp(interface, zkde_screencast_unstable_v1_interface.name) == 0 &&
               !state->screencast) {
        /* 用 v5：仍会收到 created(node)，且不依赖 v6 的 object serial。 */
        uint32_t bind_version = version < 5 ? version : 5;
        state->screencast =
            wl_registry_bind(registry, name, &zkde_screencast_unstable_v1_interface, bind_version);
    }
}

static void registry_global_remove(void *data, struct wl_registry *registry, uint32_t name)
{
    (void)data; (void)registry; (void)name;
}

static const struct wl_registry_listener registry_listener = {
    .global = registry_global,
    .global_remove = registry_global_remove,
};

int lan_video_kwin_capture(const char *window_uuid, int pointer_mode)
{
    signal(SIGTERM, on_signal);
    signal(SIGINT, on_signal);
    signal(SIGPIPE, SIG_IGN);

    struct capture_state state;
    memset(&state, 0, sizeof(state));
    state.display = wl_display_connect(NULL);
    if (!state.display) {
        fprintf(stderr, "kwin-capture: 无法连接 Wayland display（检查 WAYLAND_DISPLAY/XDG_RUNTIME_DIR）\n");
        return 2;
    }

    state.registry = wl_display_get_registry(state.display);
    wl_registry_add_listener(state.registry, &registry_listener, &state);
    if (wl_display_roundtrip(state.display) < 0) {
        fprintf(stderr, "kwin-capture: Wayland roundtrip 失败\n");
        wl_display_disconnect(state.display);
        return 2;
    }

    if (!state.screencast) {
        fprintf(stderr,
                "kwin-capture: 没有 zkde_screencast_unstable_v1（检查 desktop 文件的 "
                "X-KDE-Wayland-Interfaces 与 kbuildsycoca6）\n");
        wl_display_disconnect(state.display);
        return 3;
    }
    if (window_uuid && window_uuid[0]) {
        /* 抓单个窗口 */
        state.stream = zkde_screencast_unstable_v1_stream_window(
            state.screencast, window_uuid, (uint32_t)pointer_mode);
    } else {
        /* 抓整个输出 */
        if (!state.output) {
            fprintf(stderr, "kwin-capture: 没有可用的 wl_output\n");
            wl_display_disconnect(state.display);
            return 4;
        }
        state.stream = zkde_screencast_unstable_v1_stream_output(
            state.screencast, state.output, (uint32_t)pointer_mode);
    }
    if (!state.stream) {
        fprintf(stderr, "kwin-capture: 创建 stream 失败\n");
        wl_display_disconnect(state.display);
        return 6;
    }
    zkde_screencast_stream_unstable_v1_add_listener(state.stream, &stream_listener, &state);
    wl_display_flush(state.display);

    while (g_running && !state.failed) {
        if (wl_display_dispatch_pending(state.display) < 0) {
            break;
        }
        if (wl_display_flush(state.display) < 0 && errno != EAGAIN) {
            break;
        }
        struct pollfd pfd = {
            .fd = wl_display_get_fd(state.display),
            .events = POLLIN,
        };
        int ret = poll(&pfd, 1, 100);
        if (ret < 0) {
            if (errno == EINTR) {
                continue;
            }
            break;
        }
        if (ret == 0) {
            continue;
        }
        if (pfd.revents & (POLLERR | POLLHUP)) {
            break;
        }
        if ((pfd.revents & POLLIN) && wl_display_dispatch(state.display) < 0) {
            break;
        }
    }

    if (state.stream) {
        zkde_screencast_stream_unstable_v1_close(state.stream);
    }
    if (state.screencast) {
        zkde_screencast_unstable_v1_destroy(state.screencast);
    }
    wl_display_flush(state.display);
    wl_display_disconnect(state.display);
    return state.got_node ? 0 : 5;
}
