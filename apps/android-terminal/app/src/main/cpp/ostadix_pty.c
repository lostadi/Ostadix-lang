#define _GNU_SOURCE 1

#include <jni.h>

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <pty.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/prctl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

#define AFFINITY_TOKEN_MAGIC UINT32_C(0x4f435037)
#define PTY_READ_TIMEOUT_MILLIS 200
#define PTY_READ_TIMED_OUT (-2)

struct affinity_token {
    uint32_t magic;
    pid_t owner_tid;
    cpu_set_t previous_mask;
};

static void throw_by_name(JNIEnv *env, const char *class_name, const char *message) {
    jclass exception_class = (*env)->FindClass(env, class_name);
    if (exception_class != NULL) {
        (*env)->ThrowNew(env, exception_class, message);
        (*env)->DeleteLocalRef(env, exception_class);
    }
}

static void throw_errno(JNIEnv *env, const char *operation, int error_number) {
    char message[256];
    const char *detail = strerror(error_number);
    if (detail == NULL) {
        detail = "unknown error";
    }
    (void) snprintf(message, sizeof(message), "%s: %s", operation, detail);
    throw_by_name(env, "java/io/IOException", message);
}

static void throw_illegal_argument(JNIEnv *env, const char *message) {
    throw_by_name(env, "java/lang/IllegalArgumentException", message);
}

/* JNI's GetStringUTFChars uses modified UTF-8, which is not suitable for
 * execve paths and arguments. Convert UTF-16 to strict UTF-8 explicitly. */
static char *copy_java_string_utf8(JNIEnv *env, jstring source) {
    jsize utf16_length = (*env)->GetStringLength(env, source);
    const jchar *utf16 = (*env)->GetStringChars(env, source, NULL);
    if (utf16 == NULL) {
        return NULL;
    }

    size_t byte_length = 0U;
    for (jsize index = 0; index < utf16_length; ++index) {
        uint32_t code_point = utf16[index];
        if (code_point == 0U) {
            (*env)->ReleaseStringChars(env, source, utf16);
            throw_illegal_argument(env, "Strings passed to execve cannot contain U+0000");
            return NULL;
        }
        if (code_point >= 0xd800U && code_point <= 0xdbffU) {
            if (index + 1 >= utf16_length
                    || utf16[index + 1] < 0xdc00U || utf16[index + 1] > 0xdfffU) {
                (*env)->ReleaseStringChars(env, source, utf16);
                throw_illegal_argument(env, "String contains an unpaired UTF-16 surrogate");
                return NULL;
            }
            ++index;
            byte_length += 4U;
        } else if (code_point >= 0xdc00U && code_point <= 0xdfffU) {
            (*env)->ReleaseStringChars(env, source, utf16);
            throw_illegal_argument(env, "String contains an unpaired UTF-16 surrogate");
            return NULL;
        } else if (code_point < 0x80U) {
            byte_length += 1U;
        } else if (code_point < 0x800U) {
            byte_length += 2U;
        } else {
            byte_length += 3U;
        }
    }

    char *result = malloc(byte_length + 1U);
    if (result == NULL) {
        (*env)->ReleaseStringChars(env, source, utf16);
        throw_by_name(env, "java/lang/OutOfMemoryError", "Unable to copy UTF-8 string");
        return NULL;
    }

    size_t output = 0U;
    for (jsize index = 0; index < utf16_length; ++index) {
        uint32_t code_point = utf16[index];
        if (code_point >= 0xd800U && code_point <= 0xdbffU) {
            uint32_t low = utf16[++index];
            code_point = UINT32_C(0x10000)
                    + ((code_point - UINT32_C(0xd800)) << 10U)
                    + (low - UINT32_C(0xdc00));
        }

        if (code_point < 0x80U) {
            result[output++] = (char) code_point;
        } else if (code_point < 0x800U) {
            result[output++] = (char) (UINT32_C(0xc0) | (code_point >> 6U));
            result[output++] = (char) (UINT32_C(0x80) | (code_point & UINT32_C(0x3f)));
        } else if (code_point < 0x10000U) {
            result[output++] = (char) (UINT32_C(0xe0) | (code_point >> 12U));
            result[output++] = (char) (UINT32_C(0x80)
                    | ((code_point >> 6U) & UINT32_C(0x3f)));
            result[output++] = (char) (UINT32_C(0x80) | (code_point & UINT32_C(0x3f)));
        } else {
            result[output++] = (char) (UINT32_C(0xf0) | (code_point >> 18U));
            result[output++] = (char) (UINT32_C(0x80)
                    | ((code_point >> 12U) & UINT32_C(0x3f)));
            result[output++] = (char) (UINT32_C(0x80)
                    | ((code_point >> 6U) & UINT32_C(0x3f)));
            result[output++] = (char) (UINT32_C(0x80) | (code_point & UINT32_C(0x3f)));
        }
    }
    result[output] = '\0';
    (*env)->ReleaseStringChars(env, source, utf16);
    return result;
}

static char **copy_string_array(JNIEnv *env, jobjectArray source, jsize *length_out) {
    if (source == NULL) {
        *length_out = -1;
        return NULL;
    }

    jsize length = (*env)->GetArrayLength(env, source);
    char **result = calloc((size_t) length + 1U, sizeof(char *));
    if (result == NULL) {
        throw_by_name(env, "java/lang/OutOfMemoryError", "Unable to allocate string vector");
        return NULL;
    }

    for (jsize index = 0; index < length; ++index) {
        jstring value = (jstring) (*env)->GetObjectArrayElement(env, source, index);
        if (value == NULL) {
            throw_illegal_argument(env, "String vector contains null");
            goto fail;
        }

        result[index] = copy_java_string_utf8(env, value);
        (*env)->DeleteLocalRef(env, value);
        if (result[index] == NULL) {
            goto fail;
        }
    }

    *length_out = length;
    return result;

fail:
    for (jsize index = 0; index < length; ++index) {
        free(result[index]);
    }
    free(result);
    return NULL;
}

static void free_string_array(char **values, jsize length) {
    if (values == NULL) {
        return;
    }
    for (jsize index = 0; index < length; ++index) {
        free(values[index]);
    }
    free(values);
}

static void child_write_literal(const char *message, size_t length) {
    while (length > 0U) {
        ssize_t written = write(STDERR_FILENO, message, length);
        if (written > 0) {
            message += written;
            length -= (size_t) written;
            continue;
        }
        if (written < 0 && errno == EINTR) {
            continue;
        }
        break;
    }
}

JNIEXPORT jlong JNICALL
Java_org_ostadix_terminal_PtySession_nativeCreate(
        JNIEnv *env,
        jclass clazz,
        jstring executable,
        jobjectArray argv,
        jstring cwd,
        jobjectArray environment,
        jboolean pin_cpu7,
        jint rows,
        jint columns) {
    (void) clazz;
    if (executable == NULL || argv == NULL) {
        throw_illegal_argument(env, "executable and argv are required");
        return 0;
    }
    if (rows <= 0 || rows > USHRT_MAX || columns <= 0 || columns > USHRT_MAX) {
        throw_illegal_argument(env, "invalid terminal dimensions");
        return 0;
    }

    char *native_executable = copy_java_string_utf8(env, executable);
    if (native_executable == NULL) {
        return 0;
    }

    char *native_cwd = NULL;
    if (cwd != NULL) {
        native_cwd = copy_java_string_utf8(env, cwd);
        if (native_cwd == NULL) {
            free(native_executable);
            return 0;
        }
        if (native_cwd[0] == '\0') {
            free(native_cwd);
            native_cwd = NULL;
        }
    }

    jsize argc = 0;
    char **native_argv = copy_string_array(env, argv, &argc);
    if (native_argv == NULL) {
        free(native_cwd);
        free(native_executable);
        return 0;
    }
    if (argc == 0) {
        free_string_array(native_argv, argc);
        free(native_cwd);
        free(native_executable);
        throw_illegal_argument(env, "argv must contain argv[0]");
        return 0;
    }
    if (native_argv[0] == NULL || native_argv[0][0] == '\0') {
        free_string_array(native_argv, argc);
        free(native_cwd);
        free(native_executable);
        throw_illegal_argument(env, "argv must contain argv[0]");
        return 0;
    }

    jsize env_count = -1;
    char **native_environment = copy_string_array(env, environment, &env_count);
    if (environment != NULL && native_environment == NULL) {
        free_string_array(native_argv, argc);
        free(native_cwd);
        free(native_executable);
        return 0;
    }

    struct winsize window = {
            .ws_row = (unsigned short) rows,
            .ws_col = (unsigned short) columns,
            .ws_xpixel = 0,
            .ws_ypixel = 0,
    };

    int master_fd = -1;
    pid_t child_pid = forkpty(&master_fd, NULL, NULL, &window);
    if (child_pid == -1) {
        int saved_errno = errno;
        free_string_array(native_environment, env_count);
        free_string_array(native_argv, argc);
        free(native_cwd);
        free(native_executable);
        throw_errno(env, "forkpty", saved_errno);
        return 0;
    }

    if (child_pid == 0) {
        // forkpty/login_tty normally makes this process a session and process
        // group leader already. The call is harmless if that state is set.
        (void) setpgid(0, 0);
        (void) prctl(PR_SET_PDEATHSIG, SIGHUP, 0, 0, 0);

        if (pin_cpu7 == JNI_TRUE) {
            cpu_set_t affinity;
            CPU_ZERO(&affinity);
            CPU_SET(7, &affinity);
            // Affinity is a performance preference, not a launch condition.
            // Devices without an online logical CPU 7 still get a usable PTY.
            if (sched_setaffinity(0, sizeof(affinity), &affinity) == -1) {
                static const char warning[] =
                        "ostadix-terminal: CPU 7 unavailable; continuing unpinned\r\n";
                child_write_literal(warning, sizeof(warning) - 1U);
            }
        }

        if (native_cwd != NULL && chdir(native_cwd) == -1) {
            static const char message[] = "ostadix-terminal: unable to enter working directory\r\n";
            child_write_literal(message, sizeof(message) - 1U);
            _exit(126);
        }

        execve(
                native_executable,
                native_argv,
                native_environment == NULL ? environ : native_environment);
        int exec_errno = errno;
        static const char message[] = "ostadix-terminal: unable to execute program\r\n";
        child_write_literal(message, sizeof(message) - 1U);
        _exit(exec_errno == ENOENT ? 127 : 126);
    }

    free_string_array(native_environment, env_count);
    free_string_array(native_argv, argc);
    free(native_cwd);
    free(native_executable);

    int descriptor_flags = fcntl(master_fd, F_GETFD);
    if (descriptor_flags != -1) {
        (void) fcntl(master_fd, F_SETFD, descriptor_flags | FD_CLOEXEC);
    }

    uint64_t handle = ((uint64_t) (uint32_t) child_pid << 32U)
            | (uint64_t) (uint32_t) master_fd;
    return (jlong) handle;
}

JNIEXPORT jint JNICALL
Java_org_ostadix_terminal_PtySession_nativeRead(
        JNIEnv *env,
        jclass clazz,
        jint fd,
        jbyteArray buffer,
        jint offset,
        jint length) {
    (void) clazz;
    if (buffer == NULL || offset < 0 || length < 0
            || offset > (*env)->GetArrayLength(env, buffer) - length) {
        throw_by_name(env, "java/lang/IndexOutOfBoundsException", "invalid read buffer range");
        return -1;
    }
    if (length == 0) {
        return 0;
    }

    struct pollfd poll_descriptor = {
            .fd = fd,
            .events = POLLIN | POLLHUP | POLLERR,
            .revents = 0,
    };
    int poll_result;
    do {
        poll_result = poll(&poll_descriptor, 1U, PTY_READ_TIMEOUT_MILLIS);
    } while (poll_result == -1 && errno == EINTR);
    int poll_errno = errno;
    if (poll_result == 0) {
        return PTY_READ_TIMED_OUT;
    }
    if (poll_result == -1 || (poll_descriptor.revents & POLLNVAL) != 0) {
        throw_errno(env, "poll PTY", poll_result == -1 ? poll_errno : EBADF);
        return -1;
    }

    jbyte *bytes = (*env)->GetByteArrayElements(env, buffer, NULL);
    if (bytes == NULL) {
        return -1;
    }

    ssize_t result;
    do {
        result = read(fd, bytes + offset, (size_t) length);
    } while (result == -1 && errno == EINTR);
    int saved_errno = errno;

    (*env)->ReleaseByteArrayElements(
            env,
            buffer,
            bytes,
            result > 0 ? 0 : JNI_ABORT);

    // Linux PTY masters commonly report EIO when the final slave closes.
    if (result == 0 || (result == -1 && saved_errno == EIO)) {
        return 0;
    }
    if (result == -1) {
        throw_errno(env, "read PTY", saved_errno);
        return -1;
    }
    return (jint) result;
}

JNIEXPORT void JNICALL
Java_org_ostadix_terminal_PtySession_nativeWrite(
        JNIEnv *env,
        jclass clazz,
        jint fd,
        jbyteArray data,
        jint offset,
        jint length) {
    (void) clazz;
    if (data == NULL || offset < 0 || length < 0
            || offset > (*env)->GetArrayLength(env, data) - length) {
        throw_by_name(env, "java/lang/IndexOutOfBoundsException", "invalid write buffer range");
        return;
    }
    if (length == 0) {
        return;
    }

    jbyte *bytes = (*env)->GetByteArrayElements(env, data, NULL);
    if (bytes == NULL) {
        return;
    }

    size_t written_total = 0U;
    int saved_errno = 0;
    while (written_total < (size_t) length) {
        ssize_t written = write(
                fd,
                bytes + offset + written_total,
                (size_t) length - written_total);
        if (written > 0) {
            written_total += (size_t) written;
            continue;
        }
        if (written == -1 && errno == EINTR) {
            continue;
        }
        saved_errno = written == 0 ? EIO : errno;
        break;
    }

    (*env)->ReleaseByteArrayElements(env, data, bytes, JNI_ABORT);
    if (saved_errno != 0) {
        throw_errno(env, "write PTY", saved_errno);
    }
}

JNIEXPORT void JNICALL
Java_org_ostadix_terminal_PtySession_nativeResize(
        JNIEnv *env,
        jclass clazz,
        jint fd,
        jint pid,
        jint rows,
        jint columns) {
    (void) clazz;
    if (rows <= 0 || rows > USHRT_MAX || columns <= 0 || columns > USHRT_MAX) {
        throw_illegal_argument(env, "invalid terminal dimensions");
        return;
    }

    struct winsize window = {
            .ws_row = (unsigned short) rows,
            .ws_col = (unsigned short) columns,
            .ws_xpixel = 0,
            .ws_ypixel = 0,
    };
    if (ioctl(fd, TIOCSWINSZ, &window) == -1) {
        throw_errno(env, "resize PTY", errno);
        return;
    }

    // TIOCSWINSZ normally signals the foreground process group itself. Send
    // SIGWINCH explicitly as well so programs started before tcsetpgrp still
    // learn the initial dimensions.
    if (pid > 0 && kill(-pid, SIGWINCH) == -1
            && kill(pid, SIGWINCH) == -1 && errno != ESRCH) {
        throw_errno(env, "signal terminal resize", errno);
    }
}

JNIEXPORT void JNICALL
Java_org_ostadix_terminal_PtySession_nativeSignal(
        JNIEnv *env,
        jclass clazz,
        jint pid,
        jint signal_number) {
    (void) clazz;
    if (pid <= 0 || signal_number <= 0 || signal_number > 64) {
        throw_illegal_argument(env, "invalid pid or signal");
        return;
    }

    if (kill(-pid, signal_number) == 0) {
        return;
    }
    int group_errno = errno;
    if (kill(pid, signal_number) == 0 || errno == ESRCH) {
        return;
    }
    throw_errno(env, "signal child process", errno != 0 ? errno : group_errno);
}

JNIEXPORT jint JNICALL
Java_org_ostadix_terminal_PtySession_nativeWait(
        JNIEnv *env,
        jclass clazz,
        jint pid) {
    (void) clazz;
    if (pid <= 0) {
        throw_illegal_argument(env, "invalid pid");
        return -1;
    }

    int status = 0;
    pid_t result;
    do {
        result = waitpid(pid, &status, 0);
    } while (result == -1 && errno == EINTR);
    if (result == -1) {
        throw_errno(env, "wait for child process", errno);
        return -1;
    }
    return (jint) status;
}

JNIEXPORT void JNICALL
Java_org_ostadix_terminal_PtySession_nativeClose(
        JNIEnv *env,
        jclass clazz,
        jint fd) {
    (void) env;
    (void) clazz;
    if (fd >= 0) {
        // Do not retry close after EINTR: the descriptor may already have been
        // released and reused by another thread.
        (void) close(fd);
    }
}

JNIEXPORT jint JNICALL
Java_org_ostadix_terminal_PtySession_nativeDuplicate(
        JNIEnv *env,
        jclass clazz,
        jint fd) {
    (void) clazz;
    int duplicate;
    do {
        duplicate = fcntl(fd, F_DUPFD_CLOEXEC, 0);
    } while (duplicate == -1 && errno == EINTR);
    if (duplicate == -1) {
        throw_errno(env, "duplicate PTY descriptor", errno);
        return -1;
    }
    return duplicate;
}

JNIEXPORT jlong JNICALL
Java_org_ostadix_terminal_PtySession_nativePinCurrentThreadToCpu7(
        JNIEnv *env,
        jclass clazz) {
    (void) clazz;
    cpu_set_t previous_mask;
    if (sched_getaffinity(0, sizeof(previous_mask), &previous_mask) == -1) {
        throw_errno(env, "capture current CPU affinity", errno);
        return 0;
    }
    if (!CPU_ISSET(7, &previous_mask)) {
        throw_by_name(
                env,
                "java/io/IOException",
                "CPU 7 is not available in the calling thread's current CPU set");
        return 0;
    }

    struct affinity_token *token = calloc(1U, sizeof(*token));
    if (token == NULL) {
        throw_by_name(env, "java/lang/OutOfMemoryError", "Unable to save CPU affinity");
        return 0;
    }
    token->magic = AFFINITY_TOKEN_MAGIC;
    token->owner_tid = gettid();
    token->previous_mask = previous_mask;

    cpu_set_t cpu7_mask;
    CPU_ZERO(&cpu7_mask);
    CPU_SET(7, &cpu7_mask);
    if (sched_setaffinity(0, sizeof(cpu7_mask), &cpu7_mask) == -1) {
        int saved_errno = errno;
        token->magic = 0U;
        free(token);
        throw_errno(env, "pin current worker thread to CPU 7", saved_errno);
        return 0;
    }
    return (jlong) (uintptr_t) token;
}

JNIEXPORT void JNICALL
Java_org_ostadix_terminal_PtySession_nativeRestoreCurrentThreadAffinity(
        JNIEnv *env,
        jclass clazz,
        jlong native_token) {
    (void) clazz;
    struct affinity_token *token = (struct affinity_token *) (uintptr_t) native_token;
    if (token == NULL || token->magic != AFFINITY_TOKEN_MAGIC) {
        throw_illegal_argument(env, "invalid or already-restored CPU affinity token");
        return;
    }
    if (token->owner_tid != gettid()) {
        throw_by_name(
                env,
                "java/io/IOException",
                "CPU affinity must be restored on the worker thread that captured it");
        return;
    }

    int restore_result = sched_setaffinity(0, sizeof(token->previous_mask), &token->previous_mask);
    int saved_errno = errno;
    token->magic = 0U;
    free(token);
    if (restore_result == -1) {
        throw_errno(env, "restore worker thread CPU affinity", saved_errno);
    }
}
