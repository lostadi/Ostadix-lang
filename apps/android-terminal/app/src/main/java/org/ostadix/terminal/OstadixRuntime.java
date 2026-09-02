package org.ostadix.terminal;

import org.json.JSONException;
import org.json.JSONObject;

/** In-process JNI wrapper around the stable Ostadix embedding API. */
public final class OstadixRuntime implements AutoCloseable {
    static {
        System.loadLibrary("ostadix_runtime");
    }

    public static final class Evaluation {
        public final boolean ok;
        public final String stage;
        public final String type;
        public final String output;
        public final String message;

        private Evaluation(boolean ok, String stage, String type, String output, String message) {
            this.ok = ok;
            this.stage = stage;
            this.type = type;
            this.output = output;
            this.message = message;
        }

        public String terminalText() {
            if (ok) {
                return "[" + type + "] " + output;
            }
            return "error (" + stage + "): " + message;
        }
    }

    private long handle;

    public OstadixRuntime(String shimDirectory) {
        handle = nativeCreate(shimDirectory);
        if (handle == 0) {
            throw new IllegalStateException("Unable to initialize the Ostadix runtime");
        }
    }

    public synchronized Evaluation evaluate(String source) {
        if (handle == 0) {
            throw new IllegalStateException("Ostadix runtime is closed");
        }
        try {
            JSONObject result = new JSONObject(nativeEvaluate(handle, source));
            boolean ok = result.optBoolean("ok", false);
            return new Evaluation(
                    ok,
                    result.optString("stage", ok ? "complete" : "runtime"),
                    result.optString("type", "value"),
                    result.optString("output", ""),
                    result.optString("message", "Unknown runtime error"));
        } catch (JSONException error) {
            return new Evaluation(false, "bridge", "", "", error.getMessage());
        }
    }

    public static String version() {
        return nativeVersion();
    }

    @Override
    public synchronized void close() {
        if (handle != 0) {
            nativeDestroy(handle);
            handle = 0;
        }
    }

    private static native long nativeCreate(String shimDirectory);
    private static native String nativeEvaluate(long handle, String source);
    private static native String nativeVersion();
    private static native void nativeDestroy(long handle);
}
