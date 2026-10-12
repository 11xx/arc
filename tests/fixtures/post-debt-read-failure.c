#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int debt_synced;
static int fail_reads;

static int open_event(const char *symbol, const char *path, int flags, mode_t mode) {
    int (*real_open)(const char *, int, ...) = dlsym(RTLD_NEXT, symbol);
    const char *directory = getenv("ARC_TEST_DEBT_EVENTS");
    if (fail_reads && directory && !strncmp(path, directory, strlen(directory))
        && (flags & O_ACCMODE) == O_RDONLY && strstr(path, ".json")) {
        errno = EIO;
        return -1;
    }
    return real_open(path, flags, mode);
}

int open(const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list args;
        va_start(args, flags);
        mode = va_arg(args, int);
        va_end(args);
    }
    return open_event("open", path, flags, mode);
}

int open64(const char *path, int flags, ...) {
    mode_t mode = 0;
    if (flags & O_CREAT) {
        va_list args;
        va_start(args, flags);
        mode = va_arg(args, int);
        va_end(args);
    }
    return open_event("open64", path, flags, mode);
}

int fsync(int fd) {
    int (*real_sync)(int) = dlsym(RTLD_NEXT, "fsync");
    int result = real_sync(fd);
    const char *directory = getenv("ARC_TEST_DEBT_EVENTS");
    char link[64], path[PATH_MAX];
    snprintf(link, sizeof(link), "/proc/self/fd/%d", fd);
    ssize_t length = readlink(link, path, sizeof(path) - 1);
    if (result || !directory || length < 0) return result;
    path[length] = '\0';
    if (!strcmp(path, directory)) {
        if (debt_synced) {
            if (getenv("ARC_TEST_FAIL_DEBT_DIRECTORY_SYNC")) {
                errno = EIO;
                return -1;
            }
            fail_reads = 1;
        }
    } else if (!strncmp(path, directory, strlen(directory))) {
        int (*real_open)(const char *, int, ...) = dlsym(RTLD_NEXT, "open64");
        int event = real_open(path, O_RDONLY);
        char body[16384];
        ssize_t count = event < 0 ? -1 : read(event, body, sizeof(body) - 1);
        if (event >= 0) close(event);
        if (count > 0) {
            body[count] = '\0';
            if (strstr(body, "debt-declared")) debt_synced = 1;
        }
    }
    return result;
}
