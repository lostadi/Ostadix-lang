package org.ostadix.terminal;

import java.nio.charset.StandardCharsets;

/** Dependency-free host smoke tests for the terminal parser and screen model. */
public final class TerminalCoreSelfTest {
    private TerminalCoreSelfTest() {
    }

    public static void main(String[] arguments) {
        testCpuPolicyDefaults();
        testTextAndCursor();
        testAnsiColors();
        testSplitUtf8();
        testOscTitle();
        testBoundedScrollback();
        testAlternateScreenRestore();
        testTerminalQueryReplies();
        testWordSelectionAndReverseExtraction();
        testSoftWrapAndWideSelection();
        testSelectionAfterHistoryEviction();
        System.out.println("Terminal core self-tests passed");
    }

    private static void testCpuPolicyDefaults() {
        AppPreferences.Snapshot defaults = AppPreferences.defaults();
        equal(AppPreferences.CPU_MODE_BALANCED, defaults.cpuMode,
                "graph-safe default CPU policy");
        if (defaults.isPrimeCpu7Enabled()) {
            throw new AssertionError("default CPU policy must not pin graph workers to CPU7");
        }
        equal(
                AppPreferences.CPU_MODE_BALANCED,
                AppPreferences.migrateLegacyCpuMode(1, AppPreferences.CPU_MODE_PRIME_CPU7),
                "legacy CPU7 default safety migration");
        equal(
                AppPreferences.CPU_MODE_PRIME_CPU7,
                AppPreferences.migrateLegacyCpuMode(2, AppPreferences.CPU_MODE_PRIME_CPU7),
                "current explicit Prime policy preservation");

        AppPreferences.Snapshot explicitPrime = new AppPreferences.Snapshot(
                defaults.theme,
                defaults.fontSizeSp,
                defaults.scrollbackLines,
                defaults.cursorStyle,
                AppPreferences.CPU_MODE_PRIME_CPU7,
                defaults.keepScreenAwake,
                defaults.hapticsEnabled,
                defaults.startupMode
        );
        if (!explicitPrime.isPrimeCpu7Enabled()) {
            throw new AssertionError("explicit Prime CPU7 policy was not preserved");
        }
    }

    private static void testTextAndCursor() {
        TerminalBuffer buffer = new TerminalBuffer(12, 4, 20);
        AnsiParser parser = new AnsiParser(buffer);
        parser.feed("abc\r\nZ".getBytes(StandardCharsets.UTF_8));
        TerminalBuffer.Snapshot screen = buffer.snapshot(0);
        equal((int) 'a', screen.codePointAt(0, 0), "first character");
        equal((int) 'c', screen.codePointAt(0, 2), "third character");
        equal((int) 'Z', screen.codePointAt(1, 0), "next line");
        equal(1, screen.cursorColumn, "cursor column");
        equal(1, screen.cursorRow, "cursor row");
    }

    private static void testAnsiColors() {
        TerminalBuffer buffer = new TerminalBuffer(12, 4, 20);
        AnsiParser parser = new AnsiParser(buffer);
        parser.feed("\u001b[31mR\u001b[38;5;196mX".getBytes(StandardCharsets.UTF_8));
        TerminalBuffer.Snapshot screen = buffer.snapshot(0);
        equal(TerminalBuffer.indexedColor(1),
                screen.foregroundSpecificationAt(0, 0), "ANSI red");
        equal(TerminalBuffer.indexedColor(196),
                screen.foregroundSpecificationAt(0, 1), "ANSI 256 color");
    }

    private static void testSplitUtf8() {
        TerminalBuffer buffer = new TerminalBuffer(12, 4, 20);
        AnsiParser parser = new AnsiParser(buffer);
        byte[] encoded = "λ".getBytes(StandardCharsets.UTF_8);
        parser.feed(encoded, 0, 1);
        parser.feed(encoded, 1, encoded.length - 1);
        equal(0x03bb, buffer.snapshot(0).codePointAt(0, 0), "split UTF-8 scalar");
    }

    private static void testOscTitle() {
        TerminalBuffer buffer = new TerminalBuffer(12, 4, 20);
        AnsiParser parser = new AnsiParser(buffer);
        final String[] title = {null};
        parser.setTitleListener(new AnsiParser.TitleListener() {
            @Override
            public void onTitleChanged(String value) {
                title[0] = value;
            }
        });
        parser.feed("\u001b]0;Ostadix\u0007".getBytes(StandardCharsets.UTF_8));
        equal("Ostadix", title[0], "OSC title");
    }

    private static void testBoundedScrollback() {
        TerminalBuffer buffer = new TerminalBuffer(8, 2, 3);
        AnsiParser parser = new AnsiParser(buffer);
        parser.feed("1\r\n2\r\n3\r\n4\r\n5".getBytes(StandardCharsets.UTF_8));
        equal(3, buffer.getScrollbackSize(), "scrollback bound");
    }

    private static void testAlternateScreenRestore() {
        TerminalBuffer buffer = new TerminalBuffer(12, 4, 20);
        AnsiParser parser = new AnsiParser(buffer);
        parser.feed(("primary\u001b(0\u001b[?1049h\u001b(B\u001b[31m\u001b[?7l"
                + "\u001b[?25lfullscreen").getBytes(StandardCharsets.UTF_8));
        if (!buffer.isAlternateScreen()) {
            throw new AssertionError("alternate screen was not entered");
        }
        equal((int) 'f', buffer.snapshot(0).codePointAt(0, 0), "alternate contents");
        parser.feed("\u001b[?1049lq".getBytes(StandardCharsets.UTF_8));
        if (buffer.isAlternateScreen()) {
            throw new AssertionError("alternate screen was not exited");
        }
        equal((int) 'p', buffer.snapshot(0).codePointAt(0, 0), "restored primary contents");
        equal(0x2500, buffer.snapshot(0).codePointAt(0, 7), "restored DEC charset");
        equal(TerminalBuffer.COLOR_DEFAULT,
                buffer.snapshot(0).foregroundSpecificationAt(0, 7), "restored rendition");
        if (!buffer.snapshot(0).cursorVisible) {
            throw new AssertionError("primary cursor visibility was not restored");
        }
        equal(8, buffer.getCursorColumn(), "restored primary cursor");
    }

    private static void testTerminalQueryReplies() {
        TerminalBuffer buffer = new TerminalBuffer(12, 4, 20);
        AnsiParser parser = new AnsiParser(buffer);
        final StringBuilder replies = new StringBuilder();
        parser.setResponseListener(new AnsiParser.ResponseListener() {
            @Override
            public void onResponse(byte[] response) {
                replies.append(new String(response, StandardCharsets.UTF_8));
            }
        });
        parser.feed("ab\u001b[6n\u001b[c".getBytes(StandardCharsets.UTF_8));
        equal("\u001b[1;3R\u001b[?1;2c", replies.toString(), "terminal query replies");
    }

    private static void testWordSelectionAndReverseExtraction() {
        TerminalBuffer buffer = new TerminalBuffer(20, 3, 20);
        AnsiParser parser = new AnsiParser(buffer);
        parser.feed("run /data/app/demo\r\nnext".getBytes(StandardCharsets.UTF_8));
        TerminalBuffer.WordRange word = buffer.wordRangeAt(0, 9);
        equal(4, word.startColumn, "path word start");
        equal(17, word.endColumn, "path word end");
        equal("/data/app/demo\nnext",
                buffer.extractText(1, 3, 0, 4), "reverse multi-row selection");
    }

    private static void testSoftWrapAndWideSelection() {
        TerminalBuffer wrapped = new TerminalBuffer(4, 3, 20);
        new AnsiParser(wrapped).feed("abcdef".getBytes(StandardCharsets.UTF_8));
        equal("abcdef", wrapped.extractText(0, 0, 1, 3), "soft wrap joins rows");

        TerminalBuffer hardBreak = new TerminalBuffer(4, 3, 20);
        new AnsiParser(hardBreak).feed("ab\r\ncd".getBytes(StandardCharsets.UTF_8));
        equal("ab\ncd", hardBreak.extractText(0, 0, 1, 3), "hard break stays newline");

        TerminalBuffer wide = new TerminalBuffer(8, 2, 20);
        new AnsiParser(wide).feed("A界B".getBytes(StandardCharsets.UTF_8));
        equal("界", wide.extractText(0, 2, 0, 2), "wide continuation selects glyph");
    }

    private static void testSelectionAfterHistoryEviction() {
        TerminalBuffer buffer = new TerminalBuffer(6, 2, 3);
        new AnsiParser(buffer).feed("1\r\n2\r\n3\r\n4\r\n5".getBytes(StandardCharsets.UTF_8));
        equal(5, buffer.getDocumentLineCount(), "bounded selection document size");
        equal(3, buffer.snapshot(0).firstDocumentLine, "live viewport document origin");
        equal(1, buffer.snapshot(2).firstDocumentLine, "scrolled viewport document origin");
        equal("2\n3\n4\n5",
                buffer.extractText(4, 0, 1, 0), "selection after history eviction");
    }

    private static void equal(Object expected, Object actual, String label) {
        if (!expected.equals(actual)) {
            throw new AssertionError(label + ": expected " + expected + ", got " + actual);
        }
    }
}
