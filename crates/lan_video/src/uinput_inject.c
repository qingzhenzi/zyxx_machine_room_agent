/*
 * Linux uinput 输入注入 helper。
 *
 * 从 stdin 读取固定 5 字节记录：type(u8) code(u16 LE) value(i16 LE)
 *   type 1 = EV_KEY（键盘/鼠标键），2 = EV_REL（相对移动/滚轮）
 * 写入一个虚拟鼠标+键盘设备；由 lan-video 的 control WebSocket 喂事件。
 */

#include <errno.h>
#include <fcntl.h>
#include <linux/input.h>
#include <linux/uinput.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

static int emit(int fd, uint16_t type, uint16_t code, int32_t value)
{
    struct input_event ev;
    memset(&ev, 0, sizeof(ev));
    ev.type = type;
    ev.code = code;
    ev.value = value;
    return (int)write(fd, &ev, sizeof(ev));
}

static int emit_sync(int fd)
{
    return emit(fd, EV_SYN, SYN_REPORT, 0);
}

int lan_video_uinput_inject(void)
{
    int fd = open("/dev/uinput", O_WRONLY | O_NONBLOCK);
    if (fd < 0) {
        fprintf(stderr, "uinput: 无法打开 /dev/uinput: %s（需要 udev uaccess 规则或 input 组）\n",
                strerror(errno));
        return 2;
    }

    if (ioctl(fd, UI_SET_EVBIT, EV_KEY) < 0 ||
        ioctl(fd, UI_SET_EVBIT, EV_REL) < 0 ||
        ioctl(fd, UI_SET_EVBIT, EV_SYN) < 0) {
        fprintf(stderr, "uinput: UI_SET_EVBIT 失败: %s\n", strerror(errno));
        close(fd);
        return 3;
    }

    /* 支持所有键盘键 + 鼠标按键 */
    for (int code = 0; code <= KEY_MAX; code++) {
        ioctl(fd, UI_SET_KEYBIT, code);
    }
    const uint16_t rel_bits[] = {REL_X, REL_Y, REL_WHEEL, REL_HWHEEL};
    for (size_t i = 0; i < sizeof(rel_bits) / sizeof(rel_bits[0]); i++) {
        ioctl(fd, UI_SET_RELBIT, rel_bits[i]);
    }

    struct uinput_setup setup;
    memset(&setup, 0, sizeof(setup));
    setup.id.bustype = BUS_USB;
    setup.id.vendor = 0x1209;
    setup.id.product = 0x0001;
    setup.id.version = 1;
    snprintf(setup.name, UINPUT_MAX_NAME_SIZE, "lan-video virtual input");

    if (ioctl(fd, UI_DEV_SETUP, &setup) < 0 ||
        ioctl(fd, UI_DEV_CREATE) < 0) {
        fprintf(stderr, "uinput: 创建设备失败: %s\n", strerror(errno));
        close(fd);
        return 4;
    }
    usleep(100000);
    /* 通知 Rust 端设备已就绪，避免控制通道刚开始时丢前几个事件 */
    fputs("READY\n", stdout);
    fflush(stdout);

    uint8_t buf[5];
    size_t filled = 0;
    for (;;) {
        ssize_t n = read(STDIN_FILENO, buf + filled, sizeof(buf) - filled);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            break;
        }
        if (n == 0) {
            break;
        }
        filled += (size_t)n;
        if (filled < sizeof(buf)) {
            continue;
        }
        filled = 0;
        uint8_t type = buf[0];
        uint16_t code = (uint16_t)(buf[1] | ((uint16_t)buf[2] << 8));
        int16_t value = (int16_t)(buf[3] | ((uint16_t)buf[4] << 8));
        if (type == 1) {
            emit(fd, EV_KEY, code, value);
            emit_sync(fd);
        } else if (type == 2) {
            emit(fd, EV_REL, code, value);
            emit_sync(fd);
        }
    }

    ioctl(fd, UI_DEV_DESTROY);
    close(fd);
    return 0;
}
