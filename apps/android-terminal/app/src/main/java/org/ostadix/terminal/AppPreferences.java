package org.ostadix.terminal;

import android.content.Context;
import android.content.SharedPreferences;

import java.util.Arrays;
import java.util.Objects;

/**
 * Typed access to Ostadix Terminal settings.
 *
 * <p>All values are validated at the boundary so the terminal engine never has
 * to account for corrupt or out-of-range preferences.</p>
 */
public final class AppPreferences {
    public static final String THEME_OBSIDIAN = "obsidian";
    public static final String THEME_SOLARIZED = "solarized";
    public static final String THEME_GRAPHITE = "graphite";
    public static final String THEME_LIGHT = "light";

    public static final String CURSOR_BLOCK = "block";
    public static final String CURSOR_BAR = "bar";
    public static final String CURSOR_UNDERLINE = "underline";

    public static final String CPU_MODE_BALANCED = "balanced";
    public static final String CPU_MODE_PRIME_CPU7 = "prime_cpu7";

    public static final String STARTUP_SHELL = "shell";
    public static final String STARTUP_O_REPL = "o_repl";

    public static final int MIN_FONT_SIZE_SP = 11;
    public static final int MAX_FONT_SIZE_SP = 28;
    public static final int MIN_SCROLLBACK_LINES = 500;
    public static final int MAX_SCROLLBACK_LINES = 20_000;

    public static final String DEFAULT_THEME = THEME_OBSIDIAN;
    public static final int DEFAULT_FONT_SIZE_SP = 14;
    public static final int DEFAULT_SCROLLBACK_LINES = 5_000;
    public static final String DEFAULT_CURSOR_STYLE = CURSOR_BLOCK;
    // The graph executor creates worker threads inside the evaluator/shell
    // process. Balanced keeps the Android cpuset available to those workers;
    // Prime CPU7 remains an explicit option for known-serial workloads.
    public static final String DEFAULT_CPU_MODE = CPU_MODE_BALANCED;
    public static final boolean DEFAULT_KEEP_SCREEN_AWAKE = false;
    public static final boolean DEFAULT_HAPTICS_ENABLED = true;
    public static final String DEFAULT_STARTUP_MODE = STARTUP_SHELL;

    private static final int CURRENT_PREFERENCES_SCHEMA = 2;

    private static final String PREFERENCES_NAME = "ostadix_terminal";
    private static final String KEY_THEME = "theme";
    private static final String KEY_FONT_SIZE_SP = "font_size_sp";
    private static final String KEY_SCROLLBACK_LINES = "scrollback_lines";
    private static final String KEY_CURSOR_STYLE = "cursor_style";
    private static final String KEY_CPU_MODE = "cpu_mode";
    private static final String KEY_KEEP_SCREEN_AWAKE = "keep_screen_awake";
    private static final String KEY_HAPTICS_ENABLED = "haptics_enabled";
    private static final String KEY_STARTUP_MODE = "startup_mode";
    private static final String KEY_SCHEMA_VERSION = "preferences_schema_version";

    private static final String[] THEMES = {
            THEME_OBSIDIAN, THEME_SOLARIZED, THEME_GRAPHITE, THEME_LIGHT
    };
    private static final String[] CURSOR_STYLES = {
            CURSOR_BLOCK, CURSOR_BAR, CURSOR_UNDERLINE
    };
    private static final String[] CPU_MODES = {
            CPU_MODE_BALANCED, CPU_MODE_PRIME_CPU7
    };
    private static final String[] STARTUP_MODES = {
            STARTUP_SHELL, STARTUP_O_REPL
    };

    private final SharedPreferences preferences;

    public AppPreferences(Context context) {
        Context applicationContext = Objects.requireNonNull(context, "context")
                .getApplicationContext();
        preferences = applicationContext.getSharedPreferences(
                PREFERENCES_NAME,
                Context.MODE_PRIVATE
        );
        migratePreferences();
    }

    public String getTheme() {
        return validatedChoice(preferences.getString(KEY_THEME, DEFAULT_THEME), THEMES, DEFAULT_THEME);
    }

    public int getFontSizeSp() {
        return clamp(
                preferences.getInt(KEY_FONT_SIZE_SP, DEFAULT_FONT_SIZE_SP),
                MIN_FONT_SIZE_SP,
                MAX_FONT_SIZE_SP
        );
    }

    public int getScrollbackLines() {
        return clamp(
                preferences.getInt(KEY_SCROLLBACK_LINES, DEFAULT_SCROLLBACK_LINES),
                MIN_SCROLLBACK_LINES,
                MAX_SCROLLBACK_LINES
        );
    }

    public String getCursorStyle() {
        return validatedChoice(
                preferences.getString(KEY_CURSOR_STYLE, DEFAULT_CURSOR_STYLE),
                CURSOR_STYLES,
                DEFAULT_CURSOR_STYLE
        );
    }

    public String getCpuMode() {
        return validatedChoice(
                preferences.getString(KEY_CPU_MODE, DEFAULT_CPU_MODE),
                CPU_MODES,
                DEFAULT_CPU_MODE
        );
    }

    public boolean isPrimeCpu7Enabled() {
        return CPU_MODE_PRIME_CPU7.equals(getCpuMode());
    }

    public boolean isKeepScreenAwake() {
        return preferences.getBoolean(KEY_KEEP_SCREEN_AWAKE, DEFAULT_KEEP_SCREEN_AWAKE);
    }

    public boolean isHapticsEnabled() {
        return preferences.getBoolean(KEY_HAPTICS_ENABLED, DEFAULT_HAPTICS_ENABLED);
    }

    public String getStartupMode() {
        return validatedChoice(
                preferences.getString(KEY_STARTUP_MODE, DEFAULT_STARTUP_MODE),
                STARTUP_MODES,
                DEFAULT_STARTUP_MODE
        );
    }

    public ThemePalette getPalette() {
        return paletteFor(getTheme());
    }

    public Snapshot snapshot() {
        return new Snapshot(
                getTheme(),
                getFontSizeSp(),
                getScrollbackLines(),
                getCursorStyle(),
                getCpuMode(),
                isKeepScreenAwake(),
                isHapticsEnabled(),
                getStartupMode()
        );
    }

    /** Atomically persists one complete settings snapshot. */
    public void apply(Snapshot snapshot) {
        Snapshot safe = sanitize(Objects.requireNonNull(snapshot, "snapshot"));
        preferences.edit()
                .putInt(KEY_SCHEMA_VERSION, CURRENT_PREFERENCES_SCHEMA)
                .putString(KEY_THEME, safe.theme)
                .putInt(KEY_FONT_SIZE_SP, safe.fontSizeSp)
                .putInt(KEY_SCROLLBACK_LINES, safe.scrollbackLines)
                .putString(KEY_CURSOR_STYLE, safe.cursorStyle)
                .putString(KEY_CPU_MODE, safe.cpuMode)
                .putBoolean(KEY_KEEP_SCREEN_AWAKE, safe.keepScreenAwake)
                .putBoolean(KEY_HAPTICS_ENABLED, safe.hapticsEnabled)
                .putString(KEY_STARTUP_MODE, safe.startupMode)
                .apply();
    }

    private void migratePreferences() {
        int schema = preferences.getInt(KEY_SCHEMA_VERSION, 1);
        if (schema >= CURRENT_PREFERENCES_SCHEMA) {
            return;
        }
        String storedCpuMode = preferences.getString(KEY_CPU_MODE, null);
        String migratedCpuMode = migrateLegacyCpuMode(schema, storedCpuMode);
        SharedPreferences.Editor editor = preferences.edit()
                .putInt(KEY_SCHEMA_VERSION, CURRENT_PREFERENCES_SCHEMA);
        if (!Objects.equals(storedCpuMode, migratedCpuMode) && migratedCpuMode != null) {
            editor.putString(KEY_CPU_MODE, migratedCpuMode);
        }
        editor.apply();
    }

    /** One-time safety migration from the former CPU7 default. */
    static String migrateLegacyCpuMode(int schema, String storedCpuMode) {
        if (schema < 2 && CPU_MODE_PRIME_CPU7.equals(storedCpuMode)) {
            return CPU_MODE_BALANCED;
        }
        return storedCpuMode;
    }

    public static Snapshot defaults() {
        return new Snapshot(
                DEFAULT_THEME,
                DEFAULT_FONT_SIZE_SP,
                DEFAULT_SCROLLBACK_LINES,
                DEFAULT_CURSOR_STYLE,
                DEFAULT_CPU_MODE,
                DEFAULT_KEEP_SCREEN_AWAKE,
                DEFAULT_HAPTICS_ENABLED,
                DEFAULT_STARTUP_MODE
        );
    }

    public static String[] themeValues() {
        return THEMES.clone();
    }

    public static String[] cursorStyleValues() {
        return CURSOR_STYLES.clone();
    }

    public static String[] cpuModeValues() {
        return CPU_MODES.clone();
    }

    public static String[] startupModeValues() {
        return STARTUP_MODES.clone();
    }

    public static ThemePalette paletteFor(String theme) {
        if (THEME_SOLARIZED.equals(theme)) {
            return new ThemePalette(
                    0xFF002B36, 0xFF93A1A1, 0xFFB58900, 0xFF073642, 0xFF586E75,
                    new int[]{
                            0xFF073642, 0xFFDC322F, 0xFF859900, 0xFFB58900,
                            0xFF268BD2, 0xFFD33682, 0xFF2AA198, 0xFFEEE8D5,
                            0xFF002B36, 0xFFCB4B16, 0xFF586E75, 0xFF657B83,
                            0xFF839496, 0xFF6C71C4, 0xFF93A1A1, 0xFFFDF6E3
                    }
            );
        }
        if (THEME_GRAPHITE.equals(theme)) {
            return new ThemePalette(
                    0xFF181A1B, 0xFFE6E6E6, 0xFFFFCC66, 0xFF34373B, 0xFFA6A6A6,
                    new int[]{
                            0xFF252729, 0xFFE06C75, 0xFF98C379, 0xFFE5C07B,
                            0xFF61AFEF, 0xFFC678DD, 0xFF56B6C2, 0xFFD7DAE0,
                            0xFF5C6370, 0xFFEF596F, 0xFF89CA78, 0xFFD19A66,
                            0xFF61AFEF, 0xFFD55FDE, 0xFF2BBAC5, 0xFFFFFFFF
                    }
            );
        }
        if (THEME_LIGHT.equals(theme)) {
            return new ThemePalette(
                    0xFFF7F8FA, 0xFF20242A, 0xFF0066CC, 0xFFD9E8FA, 0xFF66717F,
                    new int[]{
                            0xFF20242A, 0xFFB42318, 0xFF287A36, 0xFF8A5B00,
                            0xFF175CD3, 0xFF9E3AA8, 0xFF087E8B, 0xFFE7E9ED,
                            0xFF66717F, 0xFFD92D20, 0xFF3B8F48, 0xFFA56F00,
                            0xFF2970D6, 0xFFB547BD, 0xFF0E9384, 0xFFFFFFFF
                    }
            );
        }
        return new ThemePalette(
                0xFF0B0F14, 0xFFE6EDF5, 0xFF67E8F9, 0xFF263445, 0xFF9AA9BA,
                new int[]{
                        0xFF111827, 0xFFFF6B6B, 0xFF7EE787, 0xFFFFC857,
                        0xFF58A6FF, 0xFFD2A8FF, 0xFF67E8F9, 0xFFDDE6F0,
                        0xFF5E6B7A, 0xFFFF8585, 0xFF9BE9A8, 0xFFFFD580,
                        0xFF79B8FF, 0xFFE0C1FF, 0xFF8AF1FF, 0xFFFFFFFF
                }
        );
    }

    private static Snapshot sanitize(Snapshot snapshot) {
        return new Snapshot(
                validatedChoice(snapshot.theme, THEMES, DEFAULT_THEME),
                clamp(snapshot.fontSizeSp, MIN_FONT_SIZE_SP, MAX_FONT_SIZE_SP),
                clamp(snapshot.scrollbackLines, MIN_SCROLLBACK_LINES, MAX_SCROLLBACK_LINES),
                validatedChoice(snapshot.cursorStyle, CURSOR_STYLES, DEFAULT_CURSOR_STYLE),
                validatedChoice(snapshot.cpuMode, CPU_MODES, DEFAULT_CPU_MODE),
                snapshot.keepScreenAwake,
                snapshot.hapticsEnabled,
                validatedChoice(snapshot.startupMode, STARTUP_MODES, DEFAULT_STARTUP_MODE)
        );
    }

    private static String validatedChoice(String candidate, String[] choices, String fallback) {
        if (candidate != null) {
            for (String choice : choices) {
                if (choice.equals(candidate)) {
                    return candidate;
                }
            }
        }
        return fallback;
    }

    private static int clamp(int value, int minimum, int maximum) {
        return Math.max(minimum, Math.min(maximum, value));
    }

    public static final class Snapshot {
        public final String theme;
        public final int fontSizeSp;
        public final int scrollbackLines;
        public final String cursorStyle;
        public final String cpuMode;
        public final boolean keepScreenAwake;
        public final boolean hapticsEnabled;
        public final String startupMode;

        public Snapshot(
                String theme,
                int fontSizeSp,
                int scrollbackLines,
                String cursorStyle,
                String cpuMode,
                boolean keepScreenAwake,
                boolean hapticsEnabled,
                String startupMode
        ) {
            this.theme = theme;
            this.fontSizeSp = fontSizeSp;
            this.scrollbackLines = scrollbackLines;
            this.cursorStyle = cursorStyle;
            this.cpuMode = cpuMode;
            this.keepScreenAwake = keepScreenAwake;
            this.hapticsEnabled = hapticsEnabled;
            this.startupMode = startupMode;
        }

        public boolean isPrimeCpu7Enabled() {
            return CPU_MODE_PRIME_CPU7.equals(cpuMode);
        }

        @Override
        public boolean equals(Object other) {
            if (this == other) {
                return true;
            }
            if (!(other instanceof Snapshot)) {
                return false;
            }
            Snapshot that = (Snapshot) other;
            return fontSizeSp == that.fontSizeSp
                    && scrollbackLines == that.scrollbackLines
                    && keepScreenAwake == that.keepScreenAwake
                    && hapticsEnabled == that.hapticsEnabled
                    && Objects.equals(theme, that.theme)
                    && Objects.equals(cursorStyle, that.cursorStyle)
                    && Objects.equals(cpuMode, that.cpuMode)
                    && Objects.equals(startupMode, that.startupMode);
        }

        @Override
        public int hashCode() {
            return Objects.hash(
                    theme,
                    fontSizeSp,
                    scrollbackLines,
                    cursorStyle,
                    cpuMode,
                    keepScreenAwake,
                    hapticsEnabled,
                    startupMode
            );
        }
    }

    public static final class ThemePalette {
        public final int background;
        public final int foreground;
        public final int cursor;
        public final int selection;
        public final int muted;
        private final int[] ansiColors;

        private ThemePalette(
                int background,
                int foreground,
                int cursor,
                int selection,
                int muted,
                int[] ansiColors
        ) {
            this.background = background;
            this.foreground = foreground;
            this.cursor = cursor;
            this.selection = selection;
            this.muted = muted;
            this.ansiColors = ansiColors.clone();
        }

        public int ansi(int index) {
            if (index < 0 || index >= ansiColors.length) {
                return foreground;
            }
            return ansiColors[index];
        }

        public int[] ansiColors() {
            return ansiColors.clone();
        }

        @Override
        public String toString() {
            return "ThemePalette{" +
                    "background=" + background +
                    ", foreground=" + foreground +
                    ", cursor=" + cursor +
                    ", selection=" + selection +
                    ", muted=" + muted +
                    ", ansiColors=" + Arrays.toString(ansiColors) +
                    '}';
        }
    }
}
