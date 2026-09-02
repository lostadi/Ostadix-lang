package org.ostadix.terminal;

import android.os.Handler;
import android.os.Looper;

import java.io.Closeable;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.Objects;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicLong;

/**
 * Owns one native pseudo-terminal and its child process.
 *
 * <p>The executable is launched directly with {@code execve}; no command is
 * interpreted by a shell unless the caller explicitly selects a shell as the
 * executable. Likewise, a root session is only created when the caller
 * explicitly launches {@code /system/bin/su}. This class never probes for
 * root.</p>
 *
 * <p>Output and lifecycle callbacks are delivered on the Android main thread.
 * Writes may block briefly and should be moved off the main thread for large
 * payloads.</p>
 */
public final class PtySession implements Closeable {
    private static final int BUFFER_SIZE = 16 * 1024;
    private static final int NATIVE_READ_TIMEOUT = -2;
    private static final int SIGHUP = 1;
    private static final int SIGTERM = 15;
    private static final int SIGKILL = 9;
    private static final long FORCE_KILL_DELAY_MILLIS = 750L;

    static {
        System.loadLibrary("ostadix_pty");
    }

    /** Receives terminal output and the final child status. */
    public interface Listener {
        /** Called with a newly allocated immutable-for-the-caller output chunk. */
        void onOutput(PtySession session, byte[] data);

        /** Called exactly once after the native child has been reaped. */
        void onExit(PtySession session, ExitStatus status);

        /** Called for an unexpected PTY or wait failure. */
        void onError(PtySession session, String message);
    }

    /** Decoded status returned by {@code waitpid(2)}. */
    public static final class ExitStatus {
        private final int exitCode;
        private final int signal;
        private final boolean coreDumped;

        private ExitStatus(int exitCode, int signal, boolean coreDumped) {
            this.exitCode = exitCode;
            this.signal = signal;
            this.coreDumped = coreDumped;
        }

        /** Returns the process exit code, or -1 if a signal terminated it. */
        public int getExitCode() {
            return exitCode;
        }

        /** Returns the terminating signal, or 0 after a normal exit. */
        public int getSignal() {
            return signal;
        }

        public boolean isCoreDumped() {
            return coreDumped;
        }

        public boolean exitedNormally() {
            return signal == 0;
        }

        @Override
        public String toString() {
            if (exitedNormally()) {
                return "exit " + exitCode;
            }
            return "signal " + signal + (coreDumped ? " (core dumped)" : "");
        }

        private static ExitStatus fromWaitStatus(int status) {
            int low = status & 0x7f;
            if (low == 0) {
                return new ExitStatus((status >>> 8) & 0xff, 0, false);
            }
            return new ExitStatus(-1, low, (status & 0x80) != 0);
        }
    }

    /**
     * Scoped CPU 7 affinity for an in-process evaluator worker.
     *
     * <p>Acquire and close this object on the same Java worker thread. The
     * original kernel affinity mask is restored by {@link #close()}. Affinity
     * is an optimization, so acquisition/restoration errors are exposed by
     * {@link #getFailureMessage()} instead of being thrown.</p>
     */
    public static final class Cpu7AffinityScope implements AutoCloseable {
        private final Thread owner;
        private final AtomicLong nativeToken;
        private final boolean acquired;
        private volatile String failureMessage;

        private Cpu7AffinityScope(long nativeToken, String failureMessage) {
            this.owner = Thread.currentThread();
            this.nativeToken = new AtomicLong(nativeToken);
            this.acquired = nativeToken != 0L;
            this.failureMessage = failureMessage;
        }

        /** Returns whether CPU 7 was successfully selected at acquisition. */
        public boolean wasPinned() {
            return acquired;
        }

        /** Returns whether this scope still owns a mask which needs restoring. */
        public boolean isActive() {
            return nativeToken.get() != 0L;
        }

        /** Returns the latest non-fatal affinity error, or null. */
        public String getFailureMessage() {
            return failureMessage;
        }

        /** Restores the captured affinity mask, returning whether it succeeded. */
        public boolean restore() {
            if (Thread.currentThread() != owner) {
                failureMessage = "CPU affinity must be restored on the worker thread that acquired it";
                return false;
            }
            long token = nativeToken.getAndSet(0L);
            if (token == 0L) {
                return failureMessage == null;
            }
            try {
                nativeRestoreCurrentThreadAffinity(token);
                return true;
            } catch (IOException exception) {
                failureMessage = exception.getMessage();
                return false;
            }
        }

        @Override
        public void close() {
            restore();
        }
    }

    private final Listener listener;
    private final Handler callbackHandler;
    private final Object fdLock = new Object();
    private final AtomicInteger masterFd;
    private final AtomicInteger processId;
    private final AtomicBoolean closeRequested = new AtomicBoolean(false);
    private final AtomicBoolean childExited = new AtomicBoolean(false);
    private final AtomicBoolean exitDelivered = new AtomicBoolean(false);

    private final Thread readerThread;
    private final Thread waiterThread;

    private PtySession(long nativeHandle, Listener listener) {
        int fd = (int) nativeHandle;
        int pid = (int) (nativeHandle >>> 32);
        if (fd < 0 || pid <= 0) {
            throw new IllegalArgumentException("Invalid native PTY handle");
        }

        this.listener = Objects.requireNonNull(listener, "listener");
        this.callbackHandler = new Handler(Looper.getMainLooper());
        this.masterFd = new AtomicInteger(fd);
        this.processId = new AtomicInteger(pid);

        readerThread = new Thread(new Runnable() {
            @Override
            public void run() {
                readLoop();
            }
        }, "Ostadix-pty-reader-" + pid);
        waiterThread = new Thread(new Runnable() {
            @Override
            public void run() {
                waitLoop();
            }
        }, "Ostadix-pty-waiter-" + pid);
        readerThread.setDaemon(true);
        waiterThread.setDaemon(true);
        readerThread.start();
        waiterThread.start();
    }

    /**
     * Launches a child connected to a new pseudo-terminal.
     *
     * @param executable absolute path to the executable
     * @param argv complete argv vector, including argv[0]
     * @param cwd child working directory, or null to inherit the app directory
     * @param environment complete {@code NAME=value} environment, or null to inherit
     * @param pinCpu7 whether the child should attempt to run only on logical CPU 7
     * @param rows initial terminal rows
     * @param columns initial terminal columns
     * @param listener callback receiver; callbacks run on the main thread
     */
    public static PtySession start(
            String executable,
            String[] argv,
            String cwd,
            String[] environment,
            boolean pinCpu7,
            int rows,
            int columns,
            Listener listener) throws IOException {
        Objects.requireNonNull(executable, "executable");
        Objects.requireNonNull(argv, "argv");
        Objects.requireNonNull(listener, "listener");
        if (executable.isEmpty()) {
            throw new IllegalArgumentException("executable must not be empty");
        }
        if (argv.length == 0 || argv[0] == null || argv[0].isEmpty()) {
            throw new IllegalArgumentException("argv must contain argv[0]");
        }
        for (String argument : argv) {
            Objects.requireNonNull(argument, "argv contains null");
        }
        if (environment != null) {
            for (String entry : environment) {
                Objects.requireNonNull(entry, "environment contains null");
                int equals = entry.indexOf('=');
                if (equals <= 0) {
                    throw new IllegalArgumentException(
                            "environment entries must have the form NAME=value");
                }
            }
        }
        validateDimensions(rows, columns);

        // Clones keep caller mutation from racing JNI argument conversion.
        long handle = nativeCreate(
                executable,
                argv.clone(),
                cwd,
                environment == null ? null : environment.clone(),
                pinCpu7,
                rows,
                columns);
        return new PtySession(handle, listener);
    }

    public int getProcessId() {
        return processId.get();
    }

    public boolean isRunning() {
        return processId.get() > 0 && !exitDelivered.get();
    }

    /**
     * Attempts to pin only the calling evaluator worker thread to CPU 7.
     *
     * <p>This method intentionally refuses calls from the Android main thread.
     * Always close the returned scope on the same worker thread, preferably
     * with try-with-resources.</p>
     */
    public static Cpu7AffinityScope tryPinCurrentThreadToCpu7() {
        if (Looper.myLooper() == Looper.getMainLooper()) {
            return new Cpu7AffinityScope(
                    0L,
                    "Refusing to pin Android's main/UI thread to CPU 7");
        }
        try {
            return new Cpu7AffinityScope(nativePinCurrentThreadToCpu7(), null);
        } catch (IOException exception) {
            return new Cpu7AffinityScope(0L, exception.getMessage());
        }
    }

    /** Writes every byte to the PTY or throws if the session is closed. */
    public void write(byte[] data) throws IOException {
        Objects.requireNonNull(data, "data");
        write(data, 0, data.length);
    }

    public void write(byte[] data, int offset, int length) throws IOException {
        Objects.requireNonNull(data, "data");
        if (offset < 0 || length < 0 || offset > data.length - length) {
            throw new IndexOutOfBoundsException("invalid write buffer range");
        }
        if (length == 0) {
            return;
        }
        final int writeFd;
        synchronized (fdLock) {
            int fd = masterFd.get();
            if (fd < 0 || closeRequested.get()) {
                throw new IOException("PTY session is closed");
            }
            // Never hold the lifecycle lock across a potentially blocking
            // PTY write. Closing or resizing the session must remain able to
            // run even if a child has stopped consuming input.
            writeFd = nativeDuplicate(fd);
        }
        try {
            nativeWrite(writeFd, data, offset, length);
        } finally {
            nativeClose(writeFd);
        }
    }

    public void writeUtf8(String text) throws IOException {
        Objects.requireNonNull(text, "text");
        write(text.getBytes(StandardCharsets.UTF_8));
    }

    /** Updates the kernel PTY size and notifies the child process group. */
    public void resize(int rows, int columns) throws IOException {
        validateDimensions(rows, columns);
        synchronized (fdLock) {
            int fd = masterFd.get();
            int pid = processId.get();
            if (fd < 0 || pid <= 0 || closeRequested.get()) {
                return;
            }
            nativeResize(fd, pid, rows, columns);
        }
    }

    /** Sends a signal to the entire child process group. */
    public void signal(int signal) throws IOException {
        if (signal <= 0 || signal > 64) {
            throw new IllegalArgumentException("invalid signal: " + signal);
        }
        int pid = processId.get();
        if (pid > 0) {
            nativeSignal(pid, signal);
        }
    }

    /** Requests graceful termination without immediately closing the PTY. */
    public void terminate() throws IOException {
        signal(SIGTERM);
    }

    /**
     * Closes the PTY and requests that the whole child process group exit.
     * A short delayed SIGKILL prevents descendants which ignore SIGHUP from
     * surviving indefinitely.
     */
    @Override
    public void close() {
        if (!closeRequested.compareAndSet(false, true)) {
            return;
        }

        int pid = processId.get();
        if (pid > 0) {
            try {
                nativeSignal(pid, SIGHUP);
            } catch (IOException ignored) {
                // It may already have exited; the waiter owns final status.
            }
        }

        synchronized (fdLock) {
            int fd = masterFd.getAndSet(-1);
            if (fd >= 0) {
                nativeClose(fd);
            }
        }

        if (pid > 0) {
            Thread forceKiller = new Thread(new Runnable() {
                @Override
                public void run() {
                    try {
                        Thread.sleep(FORCE_KILL_DELAY_MILLIS);
                    } catch (InterruptedException interrupted) {
                        Thread.currentThread().interrupt();
                        return;
                    }
                    int livePid = processId.get();
                    if (livePid == pid && closeRequested.get()) {
                        try {
                            nativeSignal(livePid, SIGKILL);
                        } catch (IOException ignored) {
                            // Exit and reaping can race this best-effort fallback.
                        }
                    }
                }
            }, "Ostadix-pty-killer-" + pid);
            forceKiller.setDaemon(true);
            forceKiller.start();
        }
    }

    private void readLoop() {
        byte[] buffer = new byte[BUFFER_SIZE];
        while (!closeRequested.get()) {
            int readFd;
            try {
                synchronized (fdLock) {
                    int fd = masterFd.get();
                    if (fd < 0 || closeRequested.get()) {
                        return;
                    }
                    // Read through a private duplicate so closing the session
                    // cannot recycle the integer descriptor under this thread.
                    readFd = nativeDuplicate(fd);
                }
            } catch (IOException exception) {
                if (!closeRequested.get()) {
                    postError("PTY descriptor duplication failed: " + exception.getMessage());
                }
                return;
            }
            final int count;
            try {
                count = nativeRead(readFd, buffer, 0, buffer.length);
            } catch (IOException exception) {
                if (!closeRequested.get()) {
                    postError("PTY read failed: " + exception.getMessage());
                }
                return;
            } finally {
                nativeClose(readFd);
            }
            if (count == NATIVE_READ_TIMEOUT) {
                if (childExited.get()) {
                    return;
                }
                continue;
            }
            if (count <= 0) {
                return;
            }
            byte[] output = Arrays.copyOf(buffer, count);
            callbackHandler.post(new Runnable() {
                @Override
                public void run() {
                    listener.onOutput(PtySession.this, output);
                }
            });
        }
    }

    private void waitLoop() {
        int pid = processId.get();
        int status;
        try {
            status = nativeWait(pid);
        } catch (IOException exception) {
            processId.compareAndSet(pid, -1);
            childExited.set(true);
            closeMasterFd();
            if (!closeRequested.get()) {
                postError("Process wait failed: " + exception.getMessage());
            }
            return;
        }

        processId.compareAndSet(pid, -1);
        childExited.set(true);
        try {
            // nativeRead polls with a short timeout, so this normally joins
            // after all PTY bytes have been queued to the main Handler.
            readerThread.join(750L);
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
        }
        closeMasterFd();
        ExitStatus exitStatus = ExitStatus.fromWaitStatus(status);
        if (exitDelivered.compareAndSet(false, true)) {
            callbackHandler.post(new Runnable() {
                @Override
                public void run() {
                    listener.onExit(PtySession.this, exitStatus);
                }
            });
        }
    }

    private void closeMasterFd() {
        synchronized (fdLock) {
            int fd = masterFd.getAndSet(-1);
            if (fd >= 0) {
                nativeClose(fd);
            }
        }
    }

    private void postError(String message) {
        callbackHandler.post(new Runnable() {
            @Override
            public void run() {
                listener.onError(PtySession.this, message);
            }
        });
    }

    private static void validateDimensions(int rows, int columns) {
        if (rows <= 0 || rows > 65535 || columns <= 0 || columns > 65535) {
            throw new IllegalArgumentException(
                    "terminal dimensions must be between 1 and 65535");
        }
    }

    private static native long nativeCreate(
            String executable,
            String[] argv,
            String cwd,
            String[] environment,
            boolean pinCpu7,
            int rows,
            int columns) throws IOException;

    private static native int nativeRead(int fd, byte[] buffer, int offset, int length)
            throws IOException;

    private static native void nativeWrite(int fd, byte[] data, int offset, int length)
            throws IOException;

    private static native void nativeResize(int fd, int pid, int rows, int columns)
            throws IOException;

    private static native void nativeSignal(int pid, int signal) throws IOException;

    private static native int nativeWait(int pid) throws IOException;

    private static native void nativeClose(int fd);

    private static native int nativeDuplicate(int fd) throws IOException;

    private static native long nativePinCurrentThreadToCpu7() throws IOException;

    private static native void nativeRestoreCurrentThreadAffinity(long token) throws IOException;
}
