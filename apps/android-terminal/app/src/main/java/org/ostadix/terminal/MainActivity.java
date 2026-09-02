package org.ostadix.terminal;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.DialogInterface;
import android.graphics.Color;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.os.Bundle;
import android.view.Gravity;
import android.view.HapticFeedbackConstants;
import android.view.View;
import android.view.ViewGroup;
import android.view.Window;
import android.view.WindowManager;
import android.widget.HorizontalScrollView;
import android.widget.LinearLayout;
import android.widget.TextView;
import android.widget.Toast;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.ThreadFactory;

/** Launcher and session coordinator for the standalone Ostadix terminal. */
public final class MainActivity extends Activity {
    private AppPreferences preferences;
    private AppFiles files;
    private TerminalView terminal;
    private TextView sessionTitle;
    private TextView sessionStatus;
    private TextView cpuBadge;
    private TextView ctrlKey;
    private TextView altKey;
    private LinearLayout rootLayout;

    private PtySession ptySession;
    private OReplController oConsole;
    private boolean rootSession;
    private boolean shellCpu7Pinned;
    private boolean ctrlEnabled;
    private boolean altEnabled;
    private AppPreferences.Snapshot settings;

    private final ExecutorService inputWriter = Executors.newSingleThreadExecutor(
            new ThreadFactory() {
                @Override
                public Thread newThread(Runnable runnable) {
                    Thread thread = new Thread(runnable, "Ostadix-terminal-writer");
                    thread.setDaemon(true);
                    return thread;
                }
            });

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        preferences = new AppPreferences(this);
        settings = preferences.snapshot();
        files = new AppFiles(this);
        try {
            files.install();
        } catch (IOException error) {
            showFatal("Unable to install bundled Ostadix files", error.getMessage());
            return;
        }

        buildInterface();
        applySettings(settings);
        if (AppPreferences.STARTUP_SHELL.equals(settings.startupMode)) {
            startShell(false);
        } else {
            startOConsole();
        }
    }

    private void buildInterface() {
        rootLayout = new LinearLayout(this);
        rootLayout.setOrientation(LinearLayout.VERTICAL);
        rootLayout.setFitsSystemWindows(true);
        setContentView(rootLayout);

        LinearLayout header = new LinearLayout(this);
        header.setGravity(Gravity.CENTER_VERTICAL);
        header.setPadding(dp(14), dp(8), dp(10), dp(8));
        rootLayout.addView(header, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                dp(58)));

        TextView logo = new TextView(this);
        logo.setText("O·");
        logo.setTextSize(24);
        logo.setTypeface(Typeface.DEFAULT_BOLD);
        logo.setGravity(Gravity.CENTER);
        header.addView(logo, new LinearLayout.LayoutParams(dp(48), dp(42)));

        LinearLayout heading = new LinearLayout(this);
        heading.setOrientation(LinearLayout.VERTICAL);
        heading.setGravity(Gravity.CENTER_VERTICAL);
        LinearLayout.LayoutParams headingParams = new LinearLayout.LayoutParams(
                0, ViewGroup.LayoutParams.MATCH_PARENT, 1f);
        headingParams.leftMargin = dp(10);
        header.addView(heading, headingParams);

        sessionTitle = new TextView(this);
        sessionTitle.setText("Ostadix Console");
        sessionTitle.setTextSize(16);
        sessionTitle.setTypeface(Typeface.DEFAULT_BOLD);
        sessionTitle.setSingleLine(true);
        heading.addView(sessionTitle);

        sessionStatus = new TextView(this);
        sessionStatus.setText("starting");
        sessionStatus.setTextSize(11);
        sessionStatus.setSingleLine(true);
        heading.addView(sessionStatus);

        cpuBadge = actionButton("CPU 7", false);
        cpuBadge.setContentDescription("O session CPU mode");
        header.addView(cpuBadge, compactButtonParams());

        TextView settingsButton = actionButton("⚙", false);
        settingsButton.setContentDescription("Terminal settings");
        settingsButton.setTextSize(19);
        settingsButton.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                showSettings();
            }
        });
        header.addView(settingsButton, compactButtonParams());

        rootLayout.addView(buildActionRow(), new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, dp(48)));

        terminal = new TerminalView(this);
        terminal.setInputListener(new TerminalView.InputListener() {
            @Override
            public void onTerminalInput(byte[] data) {
                dispatchInput(data);
            }
        });
        terminal.setResizeListener(new TerminalView.ResizeListener() {
            @Override
            public void onTerminalResize(int columns, int rows) {
                PtySession active = ptySession;
                if (active != null) {
                    try {
                        active.resize(rows, columns);
                    } catch (IOException error) {
                        showSessionError("resize", error);
                    }
                }
            }
        });
        terminal.setTitleListener(new TerminalView.TitleListener() {
            @Override
            public void onTerminalTitleChanged(String title) {
                if (ptySession != null && title != null && !title.trim().isEmpty()) {
                    sessionTitle.setText(title.trim());
                }
            }
        });
        terminal.setBellListener(new TerminalView.BellListener() {
            @Override
            public void onTerminalBell() {
                if (settings.hapticsEnabled) {
                    terminal.performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP);
                }
            }
        });
        rootLayout.addView(terminal, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f));

        rootLayout.addView(buildExtraKeys(), new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, dp(50)));
    }

    private View buildActionRow() {
        HorizontalScrollView scroll = new HorizontalScrollView(this);
        scroll.setHorizontalScrollBarEnabled(false);
        LinearLayout row = new LinearLayout(this);
        row.setGravity(Gravity.CENTER_VERTICAL);
        row.setPadding(dp(10), dp(4), dp(10), dp(4));
        scroll.addView(row, new HorizontalScrollView.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT,
                ViewGroup.LayoutParams.MATCH_PARENT));

        addAction(row, "O CONSOLE", new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                startOConsole();
            }
        }, false);
        addAction(row, "SHELL", new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                startShell(false);
            }
        }, false);
        addAction(row, "RUN EXAMPLE", new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                runExample();
            }
        }, false);
        addAction(row, "ROOT", new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                confirmRootShell();
            }
        }, true);
        addAction(row, "TERMUX", new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                confirmTermuxShell();
            }
        }, true);
        addAction(row, "SETTINGS", new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                showSettings();
            }
        }, false);
        return scroll;
    }

    private View buildExtraKeys() {
        HorizontalScrollView scroll = new HorizontalScrollView(this);
        scroll.setHorizontalScrollBarEnabled(false);
        LinearLayout keys = new LinearLayout(this);
        keys.setGravity(Gravity.CENTER_VERTICAL);
        keys.setPadding(dp(6), dp(4), dp(6), dp(4));
        scroll.addView(keys, new HorizontalScrollView.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT,
                ViewGroup.LayoutParams.MATCH_PARENT));

        addSpecialKey(keys, "ESC", TerminalView.SpecialKey.ESCAPE);
        ctrlKey = toggleKey(keys, "CTRL", true);
        altKey = toggleKey(keys, "ALT", false);
        addSpecialKey(keys, "TAB", TerminalView.SpecialKey.TAB);
        addSpecialKey(keys, "↑", TerminalView.SpecialKey.UP);
        addSpecialKey(keys, "↓", TerminalView.SpecialKey.DOWN);
        addSpecialKey(keys, "←", TerminalView.SpecialKey.LEFT);
        addSpecialKey(keys, "→", TerminalView.SpecialKey.RIGHT);
        addSpecialKey(keys, "HOME", TerminalView.SpecialKey.HOME);
        addSpecialKey(keys, "END", TerminalView.SpecialKey.END);
        addSpecialKey(keys, "PG↑", TerminalView.SpecialKey.PAGE_UP);
        addSpecialKey(keys, "PG↓", TerminalView.SpecialKey.PAGE_DOWN);
        addKey(keys, "-", "-");
        addKey(keys, "/", "/");
        addKey(keys, "|", "|");
        return scroll;
    }

    private void startOConsole() {
        stopActiveSession();
        terminal.resetTerminal();
        terminal.setTerminalResponsesEnabled(false);
        rootSession = false;
        sessionTitle.setText("Ostadix Console");
        sessionStatus.setText("native · offline · app sandbox");
        oConsole = new OReplController(files, settings.isPrimeCpu7Enabled(),
                new OReplController.Listener() {
                    @Override
                    public void onOutput(byte[] bytes) {
                        terminal.feed(bytes);
                    }

                    @Override
                    public void onBusyChanged(boolean busy) {
                        sessionStatus.setText(busy ? "evaluating" : "native · ready");
                    }

                    @Override
                    public void onRequestShell() {
                        startShell(false);
                    }

                    @Override
                    public void onRequestRootShell() {
                        confirmRootShell();
                    }

                    @Override
                    public void onRequestSettings() {
                        showSettings();
                    }

                    @Override
                    public void onConsoleClosed() {
                        startShell(false);
                    }
                });
        updateCpuBadge();
        oConsole.start();
        terminal.requestFocus();
    }

    private void startShell(boolean root) {
        startShell(root, false);
    }

    private void startShell(boolean root, boolean termux) {
        stopActiveSession();
        terminal.resetTerminal();
        terminal.setTerminalResponsesEnabled(true);
        rootSession = root;
        shellCpu7Pinned = !root && settings.isPrimeCpu7Enabled();
        final boolean pinShellCpu7 = shellCpu7Pinned;
        final boolean bundledBash = !root && files.isBashAvailable();
        final String executable = root
                ? "/system/bin/su"
                : bundledBash ? files.bashCommand().getAbsolutePath() : "/system/bin/sh";
        final String[] argv = root
                // Preserve only the exact rootEnvironment allowlist. Current
                // KernelSU otherwise replaces HOME with passwd's root home.
                ? termux
                        ? new String[] {"su", "-M", "-p", "-c", files.termuxLoginCommand()}
                        : new String[] {"su", "-p"}
                : bundledBash
                        ? new String[] {"bash", "--noprofile"}
                        : new String[] {"sh"};
        final String workingDirectory = root
                ? "/system"
                : files.workspace().getAbsolutePath();
        final String[] sessionEnvironment = root
                ? termux ? files.termuxEnvironment() : files.rootEnvironment()
                : files.nonRootEnvironment(bundledBash);
        sessionTitle.setText(root
                ? termux ? "Termux superset" : "Root shell"
                : bundledBash ? "Ostadix Bash" : "Android shell fallback");
        sessionStatus.setText(root
                ? termux ? "requesting KernelSU · full Termux environment"
                        : "requesting KernelSU grant"
                : (bundledBash ? "Bash PTY" : "system sh fallback")
                        + " · app sandbox · "
                        + (pinShellCpu7 ? "CPU7" : "balanced"));
        updateCpuBadge();
        terminal.feed((root
                ? termux
                        ? "\u001b[1;35mTERMUX SUPERSET\u001b[0m · native Termux prefix · root authority\r\n"
                                + "\u001b[2mCommands include installed Termux packages, zsh, codex, "
                                + "and user-local tools.\u001b[0m\r\n"
                        : "\u001b[1;31mROOT SESSION\u001b[0m · commands have full device authority\r\n"
                : bundledBash
                        ? "\u001b[1;38;5;81mOstadix Bash\u001b[0m · standalone GNU Bash\r\n"
                                + "\u001b[2mCommands ready: bash --version · O --help · "
                                + "O --eval '2'\u001b[0m\r\n"
                        : "\u001b[1;33mBash unavailable; using /system/bin/sh.\u001b[0m\r\n"
                                + "\u001b[2m" + files.bashUnavailableReason()
                                + "\u001b[0m\r\n")
                .getBytes(StandardCharsets.UTF_8));
        try {
            ptySession = PtySession.start(
                    executable,
                    argv,
                    workingDirectory,
                    sessionEnvironment,
                    pinShellCpu7,
                    Math.max(2, terminal.getRows()),
                    Math.max(2, terminal.getColumns()),
                    new PtySession.Listener() {
                        @Override
                        public void onOutput(PtySession session, byte[] data) {
                            if (session == ptySession) {
                                terminal.feed(data);
                                sessionStatus.setText(rootSession
                                        ? "root PTY active"
                                        : (bundledBash ? "Bash PTY active" : "system sh active")
                                                + " · "
                                                + (pinShellCpu7 ? "CPU7" : "balanced"));
                            }
                        }

                        @Override
                        public void onExit(PtySession session, PtySession.ExitStatus status) {
                            if (session == ptySession) {
                                terminal.feed(("\r\n\u001b[2m[session " + status + "]\u001b[0m\r\n")
                                        .getBytes(StandardCharsets.UTF_8));
                                sessionStatus.setText("session ended · tap SHELL or O CONSOLE");
                                ptySession = null;
                                rootSession = false;
                                shellCpu7Pinned = false;
                                updateCpuBadge();
                            }
                        }

                        @Override
                        public void onError(PtySession session, String message) {
                            if (session == ptySession) {
                                terminal.feed(("\r\n\u001b[31mPTY error:\u001b[0m " + message + "\r\n")
                                        .getBytes(StandardCharsets.UTF_8));
                                sessionStatus.setText("PTY error");
                            }
                        }
                    });
        } catch (IOException error) {
            terminal.feed(("\u001b[31mUnable to start " + executable + ":\u001b[0m "
                    + error.getMessage() + "\r\n").getBytes(StandardCharsets.UTF_8));
            sessionStatus.setText("launch failed");
            rootSession = false;
            shellCpu7Pinned = false;
            updateCpuBadge();
        }
        terminal.requestFocus();
    }

    private void runExample() {
        if (oConsole == null) {
            startOConsole();
        }
        oConsole.evaluateExample();
    }

    private void confirmRootShell() {
        new AlertDialog.Builder(this)
                .setTitle("Open a root shell?")
                .setMessage("This is an explicit privileged session. KernelSU will treat "
                        + "Ostadix Terminal as a new package and may ask you to grant it access. "
                        + "The app does not test for root at startup and does not change root "
                        + "visibility for any other app.\n\nCommands here can modify the whole device.")
                .setPositiveButton("REQUEST ROOT", new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        startShell(true);
                    }
                })
                .setNegativeButton("CANCEL", null)
                .show();
    }

    private void confirmTermuxShell() {
        new AlertDialog.Builder(this)
                .setTitle("Open the full Termux environment?")
                .setMessage("Android isolates Termux's packages from other app UIDs. This action "
                        + "uses KernelSU to enter the real Termux prefix and home, then launches "
                        + "Termux zsh with its native PATH and loader support. Every installed "
                        + "Termux command, including codex, receives root authority in this session.\n\n"
                        + "Commands can modify the whole device and Termux installation.")
                .setPositiveButton("OPEN TERMUX", new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        startShell(true, true);
                    }
                })
                .setNegativeButton("CANCEL", null)
                .show();
    }

    private void dispatchInput(byte[] data) {
        OReplController console = oConsole;
        if (console != null) {
            console.onInput(data);
            return;
        }
        final PtySession session = ptySession;
        if (session == null) {
            return;
        }
        if (rootSession && looksLikePaste(data)) {
            String preview = new String(data, StandardCharsets.UTF_8);
            if (preview.length() > 240) {
                preview = preview.substring(0, 240) + "…";
            }
            final byte[] accepted = data.clone();
            new AlertDialog.Builder(this)
                    .setTitle("Paste into root shell?")
                    .setMessage(preview)
                    .setPositiveButton("PASTE", new DialogInterface.OnClickListener() {
                        @Override
                        public void onClick(DialogInterface dialog, int which) {
                            writeToSession(session, accepted);
                        }
                    })
                    .setNegativeButton("CANCEL", null)
                    .show();
            return;
        }
        writeToSession(session, data.clone());
    }

    private void writeToSession(final PtySession session, final byte[] data) {
        inputWriter.execute(new Runnable() {
            @Override
            public void run() {
                try {
                    if (session == ptySession) {
                        session.write(data);
                    }
                } catch (IOException error) {
                    runOnUiThread(new Runnable() {
                        @Override
                        public void run() {
                            showSessionError("write", error);
                        }
                    });
                }
            }
        });
    }

    private void showSettings() {
        SettingsDialog.show(this, preferences, new SettingsDialog.Callback() {
            @Override
            public void onSettingsApplied(AppPreferences.Snapshot updated) {
                settings = updated;
                applySettings(updated);
            }
        });
    }

    private void applySettings(AppPreferences.Snapshot updated) {
        if (updated.keepScreenAwake) {
            getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        } else {
            getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        }
        AppPreferences.ThemePalette palette = AppPreferences.paletteFor(updated.theme);
        if (rootLayout != null) {
            rootLayout.setBackgroundColor(palette.background);
            terminal.applyAppearance(
                    palette,
                    updated.fontSizeSp,
                    updated.cursorStyle,
                    updated.scrollbackLines);
            sessionTitle.setTextColor(palette.foreground);
            sessionStatus.setTextColor(palette.muted);
            styleChip(cpuBadge, false);
            styleKey(ctrlKey, ctrlEnabled);
            styleKey(altKey, altEnabled);
        }
        Window window = getWindow();
        window.setStatusBarColor(palette.background);
        window.setNavigationBarColor(palette.background);
        int flags = window.getDecorView().getSystemUiVisibility();
        if (AppPreferences.THEME_LIGHT.equals(updated.theme)) {
            flags |= View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR;
            flags |= View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR;
        } else {
            flags &= ~View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR;
            flags &= ~View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR;
        }
        window.getDecorView().setSystemUiVisibility(flags);
        if (oConsole != null) {
            oConsole.setPinCpu7(updated.isPrimeCpu7Enabled());
        }
        updateCpuBadge();
    }

    private void stopActiveSession() {
        PtySession oldPty = ptySession;
        ptySession = null;
        if (oldPty != null) {
            oldPty.close();
        }
        OReplController oldConsole = oConsole;
        oConsole = null;
        if (oldConsole != null) {
            oldConsole.close();
        }
        rootSession = false;
        shellCpu7Pinned = false;
    }

    private void updateCpuBadge() {
        if (cpuBadge == null || settings == null) {
            return;
        }
        boolean pinned = (oConsole != null && settings.isPrimeCpu7Enabled())
                || shellCpu7Pinned;
        cpuBadge.setText(pinned ? "CPU 7" : "BALANCED");
        cpuBadge.setAlpha(pinned ? 1f : 0.72f);
    }

    private void addAction(LinearLayout row, String label, View.OnClickListener listener, boolean danger) {
        TextView button = actionButton(label, danger);
        button.setOnClickListener(listener);
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, dp(36));
        params.rightMargin = dp(7);
        row.addView(button, params);
    }

    private TextView actionButton(String label, boolean danger) {
        TextView button = new TextView(this);
        button.setText(label);
        button.setTextSize(11);
        button.setTypeface(Typeface.DEFAULT_BOLD);
        button.setGravity(Gravity.CENTER);
        button.setPadding(dp(12), 0, dp(12), 0);
        button.setClickable(true);
        button.setFocusable(true);
        button.setTextColor(danger ? 0xFFFF8A8A : 0xFFC7D4E4);
        GradientDrawable background = new GradientDrawable();
        background.setColor(danger ? 0x332C0E12 : 0x331A2635);
        background.setStroke(dp(1), danger ? 0x88FF6B6B : 0x554E637A);
        background.setCornerRadius(dp(10));
        button.setBackground(background);
        return button;
    }

    private void addKey(LinearLayout row, String label, final String sequence) {
        TextView key = keyView(label);
        key.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                haptic(view);
                terminal.sendInput(sequence);
            }
        });
        row.addView(key, keyParams());
    }

    private void addSpecialKey(
            LinearLayout row,
            String label,
            final TerminalView.SpecialKey specialKey) {
        TextView key = keyView(label);
        key.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                haptic(view);
                terminal.sendSpecialKey(specialKey);
            }
        });
        row.addView(key, keyParams());
    }

    private TextView toggleKey(LinearLayout row, String label, final boolean control) {
        final TextView key = keyView(label);
        key.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                haptic(view);
                if (control) {
                    ctrlEnabled = !ctrlEnabled;
                } else {
                    altEnabled = !altEnabled;
                }
                terminal.setVirtualModifiers(ctrlEnabled, altEnabled);
                styleKey(key, control ? ctrlEnabled : altEnabled);
            }
        });
        row.addView(key, keyParams());
        return key;
    }

    private TextView keyView(String label) {
        TextView key = new TextView(this);
        key.setText(label);
        key.setTextSize(12);
        key.setTypeface(Typeface.MONOSPACE, Typeface.BOLD);
        key.setGravity(Gravity.CENTER);
        key.setClickable(true);
        key.setFocusable(true);
        styleKey(key, false);
        return key;
    }

    private void styleKey(TextView key, boolean selected) {
        if (key == null || settings == null) {
            return;
        }
        AppPreferences.ThemePalette palette = AppPreferences.paletteFor(settings.theme);
        key.setTextColor(selected ? palette.background : palette.foreground);
        GradientDrawable background = new GradientDrawable();
        background.setColor(selected ? palette.cursor : palette.selection);
        background.setStroke(dp(1), selected ? palette.cursor : palette.muted);
        background.setCornerRadius(dp(8));
        key.setBackground(background);
    }

    private void styleChip(TextView chip, boolean selected) {
        if (chip == null || settings == null) {
            return;
        }
        AppPreferences.ThemePalette palette = AppPreferences.paletteFor(settings.theme);
        chip.setTextColor(palette.cursor);
        GradientDrawable background = new GradientDrawable();
        background.setColor(palette.selection);
        background.setCornerRadius(dp(10));
        chip.setBackground(background);
    }

    private void haptic(View view) {
        if (settings.hapticsEnabled) {
            view.performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP);
        }
    }

    private static boolean looksLikePaste(byte[] data) {
        if (data.length >= 96) {
            return true;
        }
        if (data.length <= 1) {
            return false;
        }
        for (byte value : data) {
            if (value == '\n' || value == '\r') {
                return true;
            }
        }
        return false;
    }

    private LinearLayout.LayoutParams compactButtonParams() {
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, dp(36));
        params.leftMargin = dp(7);
        return params;
    }

    private LinearLayout.LayoutParams keyParams() {
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(dp(58), dp(40));
        params.rightMargin = dp(6);
        return params;
    }

    private void showSessionError(String action, IOException error) {
        Toast.makeText(this, "Terminal " + action + " failed: " + error.getMessage(),
                Toast.LENGTH_SHORT).show();
    }

    private void showFatal(String title, String detail) {
        new AlertDialog.Builder(this)
                .setTitle(title)
                .setMessage(detail == null ? "Unknown error" : detail)
                .setPositiveButton("CLOSE", new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        finish();
                    }
                })
                .setCancelable(false)
                .show();
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }

    @Override
    protected void onDestroy() {
        stopActiveSession();
        inputWriter.shutdownNow();
        super.onDestroy();
    }
}
