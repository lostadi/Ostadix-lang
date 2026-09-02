package org.ostadix.terminal;

import java.lang.reflect.Field;
import java.lang.reflect.Method;

/** Host-side JNI smoke test; invokes the JSON-returning native edge directly. */
public final class RuntimeJniSmoke {
    private RuntimeJniSmoke() {
    }

    public static void main(String[] arguments) throws Exception {
        if (arguments.length != 1) {
            throw new IllegalArgumentException("expected backend directory");
        }
        OstadixRuntime runtime = new OstadixRuntime(arguments[0]);
        try {
            Field handleField = OstadixRuntime.class.getDeclaredField("handle");
            handleField.setAccessible(true);
            long handle = handleField.getLong(runtime);

            Method evaluate = OstadixRuntime.class.getDeclaredMethod(
                    "nativeEvaluate", long.class, String.class);
            evaluate.setAccessible(true);
            String response = (String) evaluate.invoke(
                    null,
                    handle,
                    "text^(Hello from JNI)_text");
            if (!response.contains("\"ok\":true")
                    || !response.contains("Hello from JNI")) {
                throw new AssertionError("unexpected JNI response: " + response);
            }
            System.out.println(response);
        } finally {
            runtime.close();
        }
    }
}
