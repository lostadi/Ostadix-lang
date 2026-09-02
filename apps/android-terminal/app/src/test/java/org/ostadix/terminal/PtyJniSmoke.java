package org.ostadix.terminal;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;

/** Device-hosted JNI smoke test for forkpty, byte I/O, and child reaping. */
public final class PtyJniSmoke {
    private PtyJniSmoke() {
    }

    public static void main(String[] arguments) throws Exception {
        Method create = method(
                "nativeCreate",
                String.class,
                String[].class,
                String.class,
                String[].class,
                boolean.class,
                int.class,
                int.class);
        Method read = method("nativeRead", int.class, byte[].class, int.class, int.class);
        Method wait = method("nativeWait", int.class);
        Method close = method("nativeClose", int.class);
        Method pinCpu7 = method("nativePinCurrentThreadToCpu7");
        Method restoreAffinity = method("nativeRestoreCurrentThreadAffinity", long.class);

        try {
            long affinityToken = (Long) pinCpu7.invoke(null);
            if (affinityToken == 0L) {
                throw new AssertionError("native CPU7 affinity returned no restoration token");
            }
            restoreAffinity.invoke(null, affinityToken);
            System.out.println("CPU7 affinity pin/restore: OK");
        } catch (InvocationTargetException exception) {
            Throwable cause = exception.getCause();
            if (!(cause instanceof IOException)
                    || cause.getMessage() == null
                    || !cause.getMessage().contains("CPU 7 is not available")) {
                throw exception;
            }
            // Android can withhold the prime core from a background Termux
            // process. This is the expected non-fatal result handled by
            // PtySession.tryPinCurrentThreadToCpu7().
            System.out.println("CPU7 affinity pin/restore: unavailable in current cpuset");
        }

        long handle = (Long) create.invoke(
                null,
                "/system/bin/sh",
                new String[] {"sh", "-c", "printf 'PTY_JNI_OK\\n'"},
                "/",
                new String[] {
                        "PATH=/system/bin:/system/xbin",
                        "HOME=/",
                        "TERM=xterm-256color",
                        "ANDROID_ROOT=/system",
                        "ANDROID_DATA=/data"
                },
                false,
                24,
                80);
        int fd = (int) handle;
        int pid = (int) (handle >>> 32);
        if (fd < 0 || pid <= 0) {
            throw new AssertionError("invalid native PTY handle");
        }

        ByteArrayOutputStream output = new ByteArrayOutputStream();
        byte[] buffer = new byte[4096];
        try {
            for (int attempts = 0; attempts < 30; attempts++) {
                int count = (Integer) read.invoke(null, fd, buffer, 0, buffer.length);
                if (count == -2) {
                    continue;
                }
                if (count <= 0) {
                    break;
                }
                output.write(buffer, 0, count);
            }
            int status = (Integer) wait.invoke(null, pid);
            if (status != 0) {
                throw new AssertionError("child wait status: " + status);
            }
        } finally {
            close.invoke(null, fd);
        }

        String text = output.toString(StandardCharsets.UTF_8.name());
        if (!text.contains("PTY_JNI_OK")) {
            throw new AssertionError("missing PTY output: " + text);
        }
        System.out.print(text);
    }

    private static Method method(String name, Class<?>... parameterTypes) throws Exception {
        Method method = PtySession.class.getDeclaredMethod(name, parameterTypes);
        method.setAccessible(true);
        return method;
    }
}
