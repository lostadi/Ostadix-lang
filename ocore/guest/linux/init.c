#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

/* PID 1 for the pinned, fully virtualized Linux probe. No private hypercall,
 * /dev/mem driver, or guest-issued O-Machine handle is involved: pread reaches
 * the built-in Linux virtio-mmio and virtio-blk drivers. */
static void report(const char *message) {
    size_t remaining = strlen(message);
    while (remaining) {
        ssize_t n = write(STDOUT_FILENO, message, remaining);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) _exit(111);
        message += n;
        remaining -= (size_t)n;
    }
}

static void idle(void) {
    for (;;) {
        struct timespec interval = {1, 0};
        nanosleep(&interval, NULL);
    }
}

static void fail(const char *where) {
    char message[256];
    snprintf(message, sizeof message, "KW LINUX FAILURE %s errno=%d\n", where, errno);
    report(message);
    idle();
}

int main(void) {
    mkdir("/dev", 0755);
    if (mount("devtmpfs", "/dev", "devtmpfs", 0, NULL) < 0 && errno != EBUSY)
        fail("mount-devtmpfs");
    int console = open("/dev/console", O_RDWR | O_NOCTTY);
    if (console < 0) return 111;
    for (int fd = 0; fd < 3; ++fd) {
        if (dup2(console, fd) < 0) return 111;
    }
    if (console > 2) close(console);
    report("KW LINUX INIT ONLINE\n");

    int disk = -1;
    for (int attempt = 0; attempt < 200 && disk < 0; ++attempt) {
        disk = open("/dev/vda", O_RDONLY | O_DIRECT | O_CLOEXEC);
        if (disk < 0) {
            struct timespec interval = {0, 50000000};
            nanosleep(&interval, NULL);
        }
    }
    if (disk < 0) fail("open-vda");
    void *allocation = NULL;
    if (posix_memalign(&allocation, 4096, 4096) != 0) fail("aligned-buffer");
    unsigned char *buffer = allocation;
    memset(buffer, 0, 4096);
    ssize_t count = pread(disk, buffer, 512, 0);
    if (count != 512) fail("initial-pread");
    for (size_t index = 0; index < 512; ++index) {
        if (buffer[index] != (unsigned char)(index ^ 0x5a)) {
            errno = EILSEQ;
            fail("block-content");
        }
    }
    report("KW LINUX BLOCK READ VERIFIED\n");
    /* The monitor observes this complete ordinary console line and arms its
     * hold before Linux can execute the following uncached request. */
    report("KW LINUX SERVICE HEALTHY\n");
    errno = 0;
    count = pread(disk, buffer, 512, 8 * 512);
    int saved_errno = errno;
    if (count != -1 || saved_errno != EIO) {
        errno = saved_errno;
        fail("withdrawal-must-return-eio");
    }
    report("KW LINUX IOERR CONSUMED\n");
    close(disk);
    free(allocation);
    /* Guest execution remains live after consuming the device-native error.
     * The monitor retains queue pages until this observation, then separately
     * decides whether to stop the vCPU and tear down its stage-2 mapping. */
    struct timespec interval = {0, 20000000};
    nanosleep(&interval, NULL);
    report("KW LINUX POST-WITHDRAWAL ALIVE\n");
    idle();
    return 0;
}
