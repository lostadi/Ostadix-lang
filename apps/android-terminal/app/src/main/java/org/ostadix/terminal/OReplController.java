package org.ostadix.terminal;

import android.os.Handler;
import android.os.Looper;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.ThreadFactory;
import java.util.concurrent.atomic.AtomicBoolean;

/** Small interactive console around the in-process Ostadix Android runtime. */
public final class OReplController implements AutoCloseable {
    private static final byte[] ARROW_UP = new byte[] {27, '[', 'A'};
    private static final byte[] ARROW_DOWN = new byte[] {27, '[', 'B'};
    private static final int MAX_SOURCE_BYTES = 1024 * 1024;
    private static final String PROMPT = "\u001b[38;5;81mO>\u001b[0m ";

    public interface Listener {
        void onOutput(byte[] bytes);
        void onBusyChanged(boolean busy);
        void onRequestShell();
        void onRequestRootShell();
        void onRequestSettings();
        void onConsoleClosed();
    }

    private final AppFiles files;
    private final String backendDirectory;
    private OstadixRuntime runtime;
    private final Listener listener;
    private final Handler mainHandler = new Handler(Looper.getMainLooper());
    private final ExecutorService evaluator = Executors.newSingleThreadExecutor(new ThreadFactory() {
        @Override
        public Thread newThread(Runnable runnable) {
            Thread thread = new Thread(runnable, "Ostadix-evaluator");
            thread.setDaemon(true);
            return thread;
        }
    });
    private final AtomicBoolean closed = new AtomicBoolean(false);
    private final AtomicBoolean busy = new AtomicBoolean(false);
    private final AtomicBoolean runtimeResetRequested = new AtomicBoolean(false);
    private final StringBuilder line = new StringBuilder();
    private final List<String> history = new ArrayList<>();
    private int historyIndex;
    private volatile boolean pinCpu7;

    public OReplController(AppFiles files, boolean pinCpu7, Listener listener) {
        this.files = files;
        this.pinCpu7 = pinCpu7;
        this.listener = listener;
        this.backendDirectory = files.backends().getAbsolutePath();
        this.runtime = new OstadixRuntime(backendDirectory);
    }

    public void start() {
        emit("\u001b[2J\u001b[H"
                + "\u001b[1;38;5;81mOstadix Console\u001b[0m  "
                + "\u001b[2mJNI bridge " + OstadixRuntime.version() + "\u001b[0m\r\n"
                + "Native, offline evaluation · :help for commands\r\n\r\n"
                + PROMPT);
    }

    public void setPinCpu7(boolean enabled) {
        if (pinCpu7 != enabled) {
            pinCpu7 = enabled;
            // Runtime owns persistent backend actors whose threads/processes
            // inherit affinity. Recreate it before the next evaluation so a
            // Prime <-> Balanced transition cannot retain the old mask.
            runtimeResetRequested.set(true);
        }
    }

    public boolean isBusy() {
        return busy.get();
    }

    public void onInput(byte[] bytes) {
        if (bytes == null || bytes.length == 0 || closed.get()) {
            return;
        }
        if (Arrays.equals(bytes, ARROW_UP)) {
            recall(-1);
            return;
        }
        if (Arrays.equals(bytes, ARROW_DOWN)) {
            recall(1);
            return;
        }

        int printableStart = 0;
        for (int index = 0; index < bytes.length; index++) {
            int value = bytes[index] & 0xff;
            if (value >= 0x20 && value != 0x7f) {
                continue;
            }
            appendPrintable(bytes, printableStart, index - printableStart);
            printableStart = index + 1;
            switch (value) {
                case 3: // Ctrl+C
                    if (busy.get()) {
                        emit("^C \u001b[2m(in-process evaluation continues)\u001b[0m\r\n");
                        break;
                    }
                    line.setLength(0);
                    emit("^C\r\n" + PROMPT);
                    break;
                case 4: // Ctrl+D
                    if (line.length() == 0 && !busy.get()) {
                        emit("\r\n\u001b[2mbye\u001b[0m\r\n");
                        listener.onConsoleClosed();
                    }
                    break;
                case 8:
                case 127:
                    eraseLastCodePoint();
                    break;
                case 10:
                case 13:
                    if (value == 10 && index > 0 && bytes[index - 1] == 13) {
                        break;
                    }
                    submitLine();
                    break;
                case 12: // Ctrl+L
                    emit("\u001b[2J\u001b[H" + PROMPT + line);
                    break;
                default:
                    break;
            }
        }
        appendPrintable(bytes, printableStart, bytes.length - printableStart);
    }

    public void evaluateExample() {
        if (busy.get()) {
            emit("\u0007");
            return;
        }
        try {
            evaluateSource(readSource(files.helloExample()), "hello.O");
        } catch (IOException error) {
            emit("\r\n\u001b[31merror:\u001b[0m " + error.getMessage() + "\r\n" + PROMPT);
        }
    }

    public void evaluateSource(String source, String label) {
        if (source == null || closed.get()) {
            return;
        }
        if (source.getBytes(StandardCharsets.UTF_8).length > MAX_SOURCE_BYTES) {
            emit("\r\n\u001b[31merror:\u001b[0m source is larger than 1 MiB\r\n" + PROMPT);
            return;
        }
        if (!busy.compareAndSet(false, true)) {
            emit("\u0007");
            return;
        }
        listener.onBusyChanged(true);
        emit("\u001b[2m  evaluating " + safeLabel(label) + "…\u001b[0m\r\n");
        evaluator.execute(new Runnable() {
            @Override
            public void run() {
                PtySession.Cpu7AffinityScope affinity = pinCpu7
                        ? PtySession.tryPinCurrentThreadToCpu7()
                        : null;
                OstadixRuntime.Evaluation evaluated = null;
                String evaluationFailure = null;
                boolean runtimeWasReset = false;
                try {
                    if (runtimeResetRequested.getAndSet(false)) {
                        runtime.close();
                        try {
                            runtime = new OstadixRuntime(backendDirectory);
                        } catch (RuntimeException error) {
                            runtimeResetRequested.set(true);
                            throw error;
                        }
                        runtimeWasReset = true;
                    }
                    evaluated = runtime.evaluate(source);
                } catch (RuntimeException error) {
                    evaluationFailure = error.getMessage() == null
                            ? error.getClass().getSimpleName()
                            : error.getMessage();
                } catch (LinkageError error) {
                    evaluationFailure = error.getMessage() == null
                            ? error.getClass().getSimpleName()
                            : error.getMessage();
                } finally {
                    if (affinity != null) {
                        affinity.close();
                    }
                }
                final OstadixRuntime.Evaluation result = evaluated;
                final String runtimeFailure = evaluationFailure;
                final boolean reportRuntimeReset = runtimeWasReset;
                final String affinityWarning = affinity != null
                        ? affinity.getFailureMessage()
                        : null;
                mainHandler.post(new Runnable() {
                    @Override
                    public void run() {
                        if (closed.get()) {
                            return;
                        }
                        if (affinityWarning != null) {
                            emit("\u001b[33mCPU7 note:\u001b[0m " + affinityWarning + "\r\n");
                        }
                        if (reportRuntimeReset) {
                            emit("\u001b[33mCPU policy:\u001b[0m runtime restarted; "
                                    + "persistent backend state was cleared\r\n");
                        }
                        if (runtimeFailure != null) {
                            emit("\u001b[31mruntime bridge error:\u001b[0m "
                                    + runtimeFailure + "\r\n");
                        } else if (result != null && result.ok) {
                            emit("\u001b[90m[" + result.type + "]\u001b[0m "
                                    + result.output + "\r\n");
                        } else if (result != null) {
                            emit("\u001b[31merror (" + result.stage + "):\u001b[0m "
                                    + result.message + "\r\n");
                        } else {
                            emit("\u001b[31mruntime bridge error:\u001b[0m no result\r\n");
                        }
                        busy.set(false);
                        listener.onBusyChanged(false);
                        emit(PROMPT);
                    }
                });
            }
        });
    }

    private void submitLine() {
        if (busy.get()) {
            emit("\u0007");
            return;
        }
        emit("\r\n");
        String source = line.toString().trim();
        line.setLength(0);
        historyIndex = history.size();
        if (source.isEmpty()) {
            emit(PROMPT);
            return;
        }
        history.add(source);
        historyIndex = history.size();
        if (source.charAt(0) == ':') {
            runCommand(source);
        } else {
            evaluateSource(source, "input");
        }
    }

    private void runCommand(String command) {
        switch (command) {
            case ":help":
            case ":?":
                emit("\u001b[1mCommands\u001b[0m\r\n"
                        + "  :example   run the bundled offline .O file\r\n"
                        + "  :shell     open Android's system shell\r\n"
                        + "  :root      explicitly request a KernelSU root shell\r\n"
                        + "  :settings  customize this terminal\r\n"
                        + "  :clear     clear the display\r\n"
                        + "  :quit      close the O Console\r\n" + PROMPT);
                break;
            case ":example":
                evaluateExample();
                break;
            case ":shell":
                listener.onRequestShell();
                break;
            case ":root":
                listener.onRequestRootShell();
                break;
            case ":settings":
                listener.onRequestSettings();
                emit(PROMPT);
                break;
            case ":clear":
                emit("\u001b[2J\u001b[H" + PROMPT);
                break;
            case ":quit":
            case ":exit":
                listener.onConsoleClosed();
                break;
            default:
                emit("\u001b[31munknown command:\u001b[0m " + command
                        + "\r\nType :help for commands.\r\n" + PROMPT);
                break;
        }
    }

    private void recall(int direction) {
        if (busy.get() || history.isEmpty()) {
            return;
        }
        historyIndex = Math.max(0, Math.min(history.size(), historyIndex + direction));
        line.setLength(0);
        if (historyIndex < history.size()) {
            line.append(history.get(historyIndex));
        }
        emit("\r\u001b[2K" + PROMPT + line);
    }

    private void appendPrintable(byte[] bytes, int offset, int length) {
        if (length <= 0 || busy.get()) {
            return;
        }
        String text = new String(bytes, offset, length, StandardCharsets.UTF_8)
                .replace("\u001b", "");
        line.append(text);
        emit(text);
    }

    private void eraseLastCodePoint() {
        if (busy.get() || line.length() == 0) {
            return;
        }
        int start = line.offsetByCodePoints(line.length(), -1);
        line.delete(start, line.length());
        emit("\b \b");
    }

    private void emit(String text) {
        emit(text.getBytes(StandardCharsets.UTF_8));
    }

    private void emit(byte[] bytes) {
        if (!closed.get()) {
            listener.onOutput(bytes);
        }
    }

    private static String safeLabel(String label) {
        if (label == null || label.isEmpty()) {
            return "source";
        }
        StringBuilder safe = new StringBuilder();
        for (int index = 0; index < label.length() && safe.length() < 80; index++) {
            char character = label.charAt(index);
            safe.append(character >= 0x20 && character != 0x7f ? character : '?');
        }
        return safe.toString();
    }

    private static String readSource(File file) throws IOException {
        if (!file.isFile()) {
            throw new IOException("Missing bundled example: " + file.getName());
        }
        if (file.length() > MAX_SOURCE_BYTES) {
            throw new IOException("Source is larger than 1 MiB");
        }
        try (FileInputStream input = new FileInputStream(file);
             ByteArrayOutputStream output = new ByteArrayOutputStream((int) file.length())) {
            byte[] buffer = new byte[16 * 1024];
            int read;
            while ((read = input.read(buffer)) != -1) {
                output.write(buffer, 0, read);
                if (output.size() > MAX_SOURCE_BYTES) {
                    throw new IOException("Source is larger than 1 MiB");
                }
            }
            return output.toString(StandardCharsets.UTF_8.name());
        }
    }

    @Override
    public void close() {
        if (!closed.compareAndSet(false, true)) {
            return;
        }
        evaluator.execute(new Runnable() {
            @Override
            public void run() {
                runtime.close();
            }
        });
        evaluator.shutdown();
    }
}
