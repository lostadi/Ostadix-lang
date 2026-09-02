package org.ostadix.terminal;

import java.util.ArrayDeque;
import java.util.Arrays;

/**
 * Thread-safe fixed-grid terminal state with bounded scrollback.
 *
 * <p>The buffer deliberately stores color specifications instead of resolved colors. That lets a
 * caller change the palette without losing the original ANSI color indexes already on screen.</p>
 */
public final class TerminalBuffer {
    public static final int MAX_COLUMNS = 512;
    public static final int MAX_ROWS = 256;
    public static final int MAX_SCROLLBACK_LINES = 50_000;
    private static final int MAX_EXTRACTED_TEXT_CHARS = 2_000_000;

    public static final int COLOR_DEFAULT = 0;
    private static final int COLOR_INDEXED = 0x01000000;
    private static final int COLOR_RGB = 0x02000000;
    private static final int COLOR_KIND_MASK = 0x7f000000;

    public static final byte STYLE_BOLD = 1;
    public static final byte STYLE_FAINT = 1 << 1;
    public static final byte STYLE_ITALIC = 1 << 2;
    public static final byte STYLE_UNDERLINE = 1 << 3;
    public static final byte STYLE_BLINK = 1 << 4;
    public static final byte STYLE_INVERSE = 1 << 5;
    public static final byte STYLE_INVISIBLE = 1 << 6;
    public static final byte STYLE_STRIKE = (byte) (1 << 7);

    private static final byte CELL_WIDE_CONTINUATION = 1;

    public enum CursorStyle {
        BLOCK,
        UNDERLINE,
        BAR
    }

    /** Inclusive column range returned for terminal word selection. */
    public static final class WordRange {
        public final int documentLine;
        public final int startColumn;
        public final int endColumn;

        private WordRange(int documentLine, int startColumn, int endColumn) {
            this.documentLine = documentLine;
            this.startColumn = startColumn;
            this.endColumn = endColumn;
        }
    }

    /** Immutable terminal palette. All colors are opaque ARGB values. */
    public static final class Palette {
        private final int[] ansi16;
        private final int foreground;
        private final int background;
        private final int cursor;

        public Palette(int[] ansi16, int foreground, int background, int cursor) {
            if (ansi16 == null || ansi16.length != 16) {
                throw new IllegalArgumentException("ansi16 must contain exactly 16 colors");
            }
            this.ansi16 = new int[16];
            for (int i = 0; i < ansi16.length; i++) {
                this.ansi16[i] = opaque(ansi16[i]);
            }
            this.foreground = opaque(foreground);
            this.background = opaque(background);
            this.cursor = opaque(cursor);
        }

        public static Palette defaultDark() {
            return new Palette(
                    new int[] {
                        0xff1b1f23, 0xffe06c75, 0xff98c379, 0xffe5c07b,
                        0xff61afef, 0xffc678dd, 0xff56b6c2, 0xffabb2bf,
                        0xff5c6370, 0xffe88388, 0xffa9d18e, 0xfff0d38a,
                        0xff78b9f2, 0xffd291e4, 0xff70c4cf, 0xfff2f4f8
                    },
                    0xffe6edf3,
                    0xff0d1117,
                    0xff58a6ff);
        }

        public int[] ansi16() {
            return ansi16.clone();
        }

        public int foreground() {
            return foreground;
        }

        public int background() {
            return background;
        }

        public int cursor() {
            return cursor;
        }

        int resolve(int specification, boolean isForeground) {
            int kind = specification & COLOR_KIND_MASK;
            if (specification == COLOR_DEFAULT) {
                return isForeground ? foreground : background;
            }
            if (kind == COLOR_RGB) {
                return 0xff000000 | (specification & 0x00ffffff);
            }
            if (kind == COLOR_INDEXED) {
                int index = specification & 0xff;
                if (index < 16) {
                    return ansi16[index];
                }
                if (index < 232) {
                    int cube = index - 16;
                    int r = xtermCube(cube / 36);
                    int g = xtermCube((cube / 6) % 6);
                    int b = xtermCube(cube % 6);
                    return 0xff000000 | (r << 16) | (g << 8) | b;
                }
                int gray = 8 + (index - 232) * 10;
                return 0xff000000 | (gray << 16) | (gray << 8) | gray;
            }
            return isForeground ? foreground : background;
        }

        private static int xtermCube(int component) {
            return component == 0 ? 0 : 55 + component * 40;
        }

        private static int opaque(int color) {
            return 0xff000000 | (color & 0x00ffffff);
        }
    }

    /** Immutable copy used by the renderer without holding the buffer lock. */
    public static final class Snapshot {
        public final int columns;
        public final int rows;
        public final int cursorColumn;
        public final int cursorRow;
        public final boolean cursorVisible;
        public final CursorStyle cursorStyle;
        public final int scrollOffset;
        public final int scrollbackLines;
        /** Zero-based line in the complete scrollback-plus-screen document shown at row zero. */
        public final int firstDocumentLine;
        public final int documentLines;
        public final long generation;

        private final int[][] codePoints;
        private final int[][] foregrounds;
        private final int[][] backgrounds;
        private final byte[][] styles;
        private final byte[][] cellFlags;
        private final Palette palette;

        private Snapshot(
                int columns,
                int rows,
                int cursorColumn,
                int cursorRow,
                boolean cursorVisible,
                CursorStyle cursorStyle,
                int scrollOffset,
                int scrollbackLines,
                int firstDocumentLine,
                int documentLines,
                long generation,
                int[][] codePoints,
                int[][] foregrounds,
                int[][] backgrounds,
                byte[][] styles,
                byte[][] cellFlags,
                Palette palette) {
            this.columns = columns;
            this.rows = rows;
            this.cursorColumn = cursorColumn;
            this.cursorRow = cursorRow;
            this.cursorVisible = cursorVisible;
            this.cursorStyle = cursorStyle;
            this.scrollOffset = scrollOffset;
            this.scrollbackLines = scrollbackLines;
            this.firstDocumentLine = firstDocumentLine;
            this.documentLines = documentLines;
            this.generation = generation;
            this.codePoints = codePoints;
            this.foregrounds = foregrounds;
            this.backgrounds = backgrounds;
            this.styles = styles;
            this.cellFlags = cellFlags;
            this.palette = palette;
        }

        public int codePointAt(int row, int column) {
            return codePoints[row][column];
        }

        public int foregroundAt(int row, int column) {
            return palette.resolve(foregrounds[row][column], true);
        }

        public int backgroundAt(int row, int column) {
            return palette.resolve(backgrounds[row][column], false);
        }

        public int foregroundSpecificationAt(int row, int column) {
            return foregrounds[row][column];
        }

        public byte styleAt(int row, int column) {
            return styles[row][column];
        }

        public boolean isWideContinuationAt(int row, int column) {
            return (cellFlags[row][column] & CELL_WIDE_CONTINUATION) != 0;
        }

        public int defaultForeground() {
            return palette.foreground();
        }

        public int defaultBackground() {
            return palette.background();
        }

        public int cursorColor() {
            return palette.cursor();
        }
    }

    private static final class Line {
        final int[] codePoints;
        final int[] foregrounds;
        final int[] backgrounds;
        final byte[] styles;
        final byte[] cellFlags;
        boolean wrapped;

        Line(int columns, int foreground, int background) {
            codePoints = new int[columns];
            foregrounds = new int[columns];
            backgrounds = new int[columns];
            styles = new byte[columns];
            cellFlags = new byte[columns];
            if (foreground != COLOR_DEFAULT) {
                Arrays.fill(foregrounds, foreground);
            }
            if (background != COLOR_DEFAULT) {
                Arrays.fill(backgrounds, background);
            }
        }

        Line copy() {
            Line result = new Line(codePoints.length, COLOR_DEFAULT, COLOR_DEFAULT);
            System.arraycopy(codePoints, 0, result.codePoints, 0, codePoints.length);
            System.arraycopy(foregrounds, 0, result.foregrounds, 0, foregrounds.length);
            System.arraycopy(backgrounds, 0, result.backgrounds, 0, backgrounds.length);
            System.arraycopy(styles, 0, result.styles, 0, styles.length);
            System.arraycopy(cellFlags, 0, result.cellFlags, 0, cellFlags.length);
            result.wrapped = wrapped;
            return result;
        }
    }

    /** Primary-screen state retained while a full-screen app owns the alternate screen. */
    private static final class ScreenState {
        int columns;
        int rows;
        Line[] screen;
        ArrayDeque<Line> scrollback;
        int cursorColumn;
        int cursorRow;
        int savedCursorColumn;
        int savedCursorRow;
        int scrollTop;
        int scrollBottom;
        boolean wrapPending;
        boolean autoWrap;
        boolean cursorVisible;
        CursorStyle cursorStyle;
        int currentForeground;
        int currentBackground;
        byte currentStyle;
    }

    private int columns;
    private int rows;
    private Line[] screen;
    private ArrayDeque<Line> scrollback = new ArrayDeque<>();
    private int scrollbackLimit;
    private ScreenState primaryScreen;
    private boolean alternateScreen;

    private int cursorColumn;
    private int cursorRow;
    private int savedCursorColumn;
    private int savedCursorRow;
    private int scrollTop;
    private int scrollBottom;
    private boolean wrapPending;
    private boolean autoWrap = true;
    private boolean cursorVisible = true;
    private CursorStyle cursorStyle = CursorStyle.BLOCK;

    private int currentForeground = COLOR_DEFAULT;
    private int currentBackground = COLOR_DEFAULT;
    private byte currentStyle;
    private Palette palette = Palette.defaultDark();
    private long generation;

    public TerminalBuffer(int columns, int rows, int scrollbackLimit) {
        this.columns = clamp(columns, 1, MAX_COLUMNS);
        this.rows = clamp(rows, 1, MAX_ROWS);
        this.scrollbackLimit = clamp(scrollbackLimit, 0, MAX_SCROLLBACK_LINES);
        this.screen = new Line[this.rows];
        for (int row = 0; row < this.rows; row++) {
            this.screen[row] = blankLine();
        }
        scrollTop = 0;
        scrollBottom = this.rows - 1;
    }

    public static int indexedColor(int index) {
        return COLOR_INDEXED | clamp(index, 0, 255);
    }

    public static int rgbColor(int red, int green, int blue) {
        return COLOR_RGB
                | (clamp(red, 0, 255) << 16)
                | (clamp(green, 0, 255) << 8)
                | clamp(blue, 0, 255);
    }

    public synchronized int getColumns() {
        return columns;
    }

    public synchronized int getRows() {
        return rows;
    }

    public synchronized int getCursorColumn() {
        return cursorColumn;
    }

    public synchronized int getCursorRow() {
        return cursorRow;
    }

    public synchronized boolean isAlternateScreen() {
        return alternateScreen;
    }

    public synchronized int getScrollbackSize() {
        return scrollback.size();
    }

    public synchronized long getGeneration() {
        return generation;
    }

    public synchronized Palette getPalette() {
        return palette;
    }

    public synchronized void setPalette(Palette palette) {
        if (palette == null) {
            throw new IllegalArgumentException("palette must not be null");
        }
        this.palette = palette;
        generation++;
    }

    public synchronized void setScrollbackLimit(int limit) {
        scrollbackLimit = clamp(limit, 0, MAX_SCROLLBACK_LINES);
        trimScrollback();
        if (primaryScreen != null) {
            trimDeque(primaryScreen.scrollback);
        }
        generation++;
    }

    public synchronized void setCursorStyle(CursorStyle style) {
        cursorStyle = style == null ? CursorStyle.BLOCK : style;
        generation++;
    }

    public synchronized CursorStyle getCursorStyle() {
        return cursorStyle;
    }

    public synchronized void setCursorVisible(boolean visible) {
        cursorVisible = visible;
        generation++;
    }

    public synchronized void setAutoWrap(boolean enabled) {
        autoWrap = enabled;
        if (!enabled) {
            wrapPending = false;
        }
    }

    /** Enter a clear alternate screen while preserving the complete primary display. */
    public synchronized void enterAlternateScreen() {
        if (alternateScreen) {
            return;
        }
        primaryScreen = captureScreen();
        alternateScreen = true;
        scrollback = new ArrayDeque<>();
        screen = new Line[rows];
        for (int row = 0; row < rows; row++) {
            screen[row] = blankLine();
        }
        cursorColumn = 0;
        cursorRow = 0;
        savedCursorColumn = 0;
        savedCursorRow = 0;
        scrollTop = 0;
        scrollBottom = rows - 1;
        wrapPending = false;
        generation++;
    }

    /** Leave the alternate screen and restore the primary display and cursor. */
    public synchronized void exitAlternateScreen() {
        if (!alternateScreen || primaryScreen == null) {
            return;
        }
        int visibleColumns = columns;
        int visibleRows = rows;
        restoreScreen(primaryScreen);
        primaryScreen = null;
        alternateScreen = false;
        trimScrollback();
        if (columns != visibleColumns || rows != visibleRows) {
            resize(visibleColumns, visibleRows);
        }
        generation++;
    }

    public synchronized void resize(int requestedColumns, int requestedRows) {
        int newColumns = clamp(requestedColumns, 1, MAX_COLUMNS);
        int newRows = clamp(requestedRows, 1, MAX_ROWS);
        if (newColumns == columns && newRows == rows) {
            return;
        }

        Line[] resizedOldScreen = new Line[rows];
        for (int row = 0; row < rows; row++) {
            resizedOldScreen[row] = resizeLine(screen[row], newColumns);
        }
        // Scrollback is lazily normalized in snapshot(); keeping its original width avoids a
        // resize cost proportional to an arbitrarily large history.

        Line[] newScreen = new Line[newRows];
        if (newRows < rows) {
            int removed = rows - newRows;
            for (int row = 0; row < removed; row++) {
                addScrollback(resizedOldScreen[row]);
            }
            System.arraycopy(resizedOldScreen, removed, newScreen, 0, newRows);
            cursorRow = Math.max(0, cursorRow - removed);
        } else {
            System.arraycopy(resizedOldScreen, 0, newScreen, 0, rows);
            for (int row = rows; row < newRows; row++) {
                newScreen[row] = new Line(newColumns, currentForeground, currentBackground);
            }
        }

        columns = newColumns;
        rows = newRows;
        screen = newScreen;
        cursorColumn = clamp(cursorColumn, 0, columns - 1);
        cursorRow = clamp(cursorRow, 0, rows - 1);
        savedCursorColumn = clamp(savedCursorColumn, 0, columns - 1);
        savedCursorRow = clamp(savedCursorRow, 0, rows - 1);
        scrollTop = 0;
        scrollBottom = rows - 1;
        wrapPending = false;
        generation++;
    }

    public synchronized Snapshot snapshot(int requestedScrollOffset) {
        int historySize = scrollback.size();
        int offset = clamp(requestedScrollOffset, 0, historySize);
        Line[] history = scrollback.toArray(new Line[0]);
        int start = historySize - offset;

        int[][] codePoints = new int[rows][columns];
        int[][] foregrounds = new int[rows][columns];
        int[][] backgrounds = new int[rows][columns];
        byte[][] styles = new byte[rows][columns];
        byte[][] cellFlags = new byte[rows][columns];
        for (int outputRow = 0; outputRow < rows; outputRow++) {
            int sourceIndex = start + outputRow;
            Line source = sourceIndex < historySize
                    ? history[sourceIndex]
                    : screen[sourceIndex - historySize];
            copyLineToSnapshot(
                    source,
                    codePoints[outputRow],
                    foregrounds[outputRow],
                    backgrounds[outputRow],
                    styles[outputRow],
                    cellFlags[outputRow]);
        }

        return new Snapshot(
                columns,
                rows,
                offset == 0 ? cursorColumn : -1,
                offset == 0 ? cursorRow : -1,
                cursorVisible,
                cursorStyle,
                offset,
                historySize,
                start,
                historySize + rows,
                generation,
                codePoints,
                foregrounds,
                backgrounds,
                styles,
                cellFlags,
                palette);
    }

    /** Number of addressable rows in the current scrollback-plus-screen document. */
    public synchronized int getDocumentLineCount() {
        return scrollback.size() + rows;
    }

    /**
     * Finds a terminal-friendly word at one document cell.
     *
     * <p>Shell/path punctuation is treated as part of a word so long-pressing a path, URL, flag,
     * or environment assignment selects something useful. Punctuation that is not commonly used
     * in those tokens forms its own contiguous class. A blank cell selects only itself.</p>
     */
    public synchronized WordRange wordRangeAt(int documentLine, int requestedColumn) {
        int historySize = scrollback.size();
        int documentLines = historySize + rows;
        if (documentLine < 0 || documentLine >= documentLines) {
            return null;
        }
        Line[] history = scrollback.toArray(new Line[0]);
        Line line = documentLineAt(documentLine, history);
        int column = clamp(requestedColumn, 0, columns - 1);
        if (isContinuation(line, column) && column > 0) {
            column--;
        }
        int codePoint = codePointAt(line, column);
        int characterClass = selectionCharacterClass(codePoint);
        if (characterClass == 0) {
            return new WordRange(documentLine, column, column);
        }

        int start = column;
        while (start > 0) {
            int candidate = start - 1;
            if (isContinuation(line, candidate) && candidate > 0) {
                candidate--;
            }
            if (selectionCharacterClass(codePointAt(line, candidate)) != characterClass) {
                break;
            }
            start = candidate;
        }

        int end = column;
        if (isWideCell(line, end)) {
            end++;
        }
        while (end + 1 < columns) {
            int candidate = end + 1;
            if (isContinuation(line, candidate)) {
                end = candidate;
                continue;
            }
            if (selectionCharacterClass(codePointAt(line, candidate)) != characterClass) {
                break;
            }
            end = candidate;
            if (isWideCell(line, end)) {
                end++;
            }
        }
        return new WordRange(documentLine, start, Math.min(columns - 1, end));
    }

    /**
     * Copies an inclusive rectangular-flow selection as plain text.
     *
     * <p>Trailing empty cells are removed, soft-wrapped rows are joined, and hard line boundaries
     * become newlines. Extremely large selections are capped before reaching Android's clipboard
     * binder.</p>
     */
    public synchronized String extractText(
            int requestedStartLine,
            int requestedStartColumn,
            int requestedEndLine,
            int requestedEndColumn) {
        int startLine = requestedStartLine;
        int startColumn = requestedStartColumn;
        int endLine = requestedEndLine;
        int endColumn = requestedEndColumn;
        if (comparePosition(startLine, startColumn, endLine, endColumn) > 0) {
            int swapLine = startLine;
            int swapColumn = startColumn;
            startLine = endLine;
            startColumn = endColumn;
            endLine = swapLine;
            endColumn = swapColumn;
        }

        int historySize = scrollback.size();
        int documentLines = historySize + rows;
        if (documentLines <= 0) {
            return "";
        }
        startLine = clamp(startLine, 0, documentLines - 1);
        endLine = clamp(endLine, startLine, documentLines - 1);
        startColumn = clamp(startColumn, 0, columns - 1);
        endColumn = clamp(endColumn, 0, columns - 1);
        Line[] history = scrollback.toArray(new Line[0]);
        Line firstSelectedLine = documentLineAt(startLine, history);
        Line lastSelectedLine = documentLineAt(endLine, history);
        if (isContinuation(firstSelectedLine, startColumn) && startColumn > 0) {
            startColumn--;
        }
        if (isContinuation(lastSelectedLine, endColumn) && endColumn > 0) {
            endColumn--;
        }
        StringBuilder result = new StringBuilder(
                Math.min(MAX_EXTRACTED_TEXT_CHARS, (endLine - startLine + 1) * columns));
        boolean truncated = false;

        for (int documentLine = startLine; documentLine <= endLine; documentLine++) {
            Line line = documentLineAt(documentLine, history);
            int from = documentLine == startLine ? startColumn : 0;
            int to = documentLine == endLine ? endColumn : columns - 1;
            StringBuilder selectedLine = new StringBuilder(Math.max(0, to - from + 1));
            for (int column = from; column <= to; column++) {
                if (isContinuation(line, column)) {
                    continue;
                }
                int codePoint = codePointAt(line, column);
                selectedLine.appendCodePoint(codePoint == 0 ? ' ' : codePoint);
            }
            while (selectedLine.length() > 0
                    && selectedLine.charAt(selectedLine.length() - 1) == ' ') {
                selectedLine.setLength(selectedLine.length() - 1);
            }
            if (result.length() + selectedLine.length() > MAX_EXTRACTED_TEXT_CHARS) {
                int remaining = MAX_EXTRACTED_TEXT_CHARS - result.length();
                if (remaining > 0) {
                    result.append(selectedLine, 0, remaining);
                }
                truncated = true;
                break;
            }
            result.append(selectedLine);
            if (documentLine < endLine && !line.wrapped) {
                if (result.length() >= MAX_EXTRACTED_TEXT_CHARS) {
                    truncated = true;
                    break;
                }
                result.append('\n');
            }
        }
        while (result.length() > 0 && result.charAt(result.length() - 1) == '\n') {
            result.setLength(result.length() - 1);
        }
        if (truncated) {
            result.append("\n…[selection truncated]");
        }
        return result.toString();
    }

    public synchronized void putCodePoint(int codePoint) {
        if (!Character.isValidCodePoint(codePoint) || isControl(codePoint)) {
            return;
        }
        if (isCombining(codePoint)) {
            // A cell stores one scalar. Ignoring a detached combining scalar is safer than letting
            // it advance the cursor or corrupt the fixed grid.
            return;
        }

        int width = isWide(codePoint) ? 2 : 1;
        if (wrapPending) {
            if (autoWrap) {
                screen[cursorRow].wrapped = true;
                cursorColumn = 0;
                lineFeedInternal();
            }
            wrapPending = false;
        }
        if (width == 2 && cursorColumn == columns - 1) {
            if (autoWrap) {
                screen[cursorRow].wrapped = true;
                cursorColumn = 0;
                lineFeedInternal();
            } else {
                width = 1;
            }
        }

        clearWidePairAt(cursorRow, cursorColumn);
        Line line = screen[cursorRow];
        line.codePoints[cursorColumn] = codePoint;
        line.foregrounds[cursorColumn] = currentForeground;
        line.backgrounds[cursorColumn] = currentBackground;
        line.styles[cursorColumn] = currentStyle;
        line.cellFlags[cursorColumn] = 0;
        if (width == 2 && cursorColumn + 1 < columns) {
            clearWidePairAt(cursorRow, cursorColumn + 1);
            line.codePoints[cursorColumn + 1] = 0;
            line.foregrounds[cursorColumn + 1] = currentForeground;
            line.backgrounds[cursorColumn + 1] = currentBackground;
            line.styles[cursorColumn + 1] = currentStyle;
            line.cellFlags[cursorColumn + 1] = CELL_WIDE_CONTINUATION;
        }

        int lastOccupied = cursorColumn + width - 1;
        if (lastOccupied >= columns - 1) {
            cursorColumn = columns - 1;
            wrapPending = autoWrap;
        } else {
            cursorColumn += width;
        }
        generation++;
    }

    public synchronized void carriageReturn() {
        cursorColumn = 0;
        wrapPending = false;
        generation++;
    }

    public synchronized void lineFeed() {
        lineFeedInternal();
        wrapPending = false;
        generation++;
    }

    public synchronized void nextLine() {
        cursorColumn = 0;
        lineFeedInternal();
        wrapPending = false;
        generation++;
    }

    public synchronized void reverseIndex() {
        wrapPending = false;
        if (cursorRow == scrollTop) {
            scrollDownInternal(scrollTop, scrollBottom, 1);
        } else {
            cursorRow = Math.max(scrollTop, cursorRow - 1);
        }
        generation++;
    }

    public synchronized void backspace() {
        wrapPending = false;
        if (cursorColumn > 0) {
            cursorColumn--;
            if ((screen[cursorRow].cellFlags[cursorColumn] & CELL_WIDE_CONTINUATION) != 0
                    && cursorColumn > 0) {
                cursorColumn--;
            }
        }
        generation++;
    }

    public synchronized void tab() {
        wrapPending = false;
        cursorColumn = Math.min(columns - 1, ((cursorColumn / 8) + 1) * 8);
        generation++;
    }

    public synchronized void moveCursor(int rowDelta, int columnDelta) {
        wrapPending = false;
        cursorRow = clamp(cursorRow + rowDelta, 0, rows - 1);
        cursorColumn = clamp(cursorColumn + columnDelta, 0, columns - 1);
        generation++;
    }

    public synchronized void setCursor(int oneBasedRow, int oneBasedColumn) {
        wrapPending = false;
        cursorRow = clamp(Math.max(1, oneBasedRow) - 1, 0, rows - 1);
        cursorColumn = clamp(Math.max(1, oneBasedColumn) - 1, 0, columns - 1);
        generation++;
    }

    public synchronized void setCursorRow(int oneBasedRow) {
        setCursor(oneBasedRow, cursorColumn + 1);
    }

    public synchronized void setCursorColumn(int oneBasedColumn) {
        setCursor(cursorRow + 1, oneBasedColumn);
    }

    public synchronized void saveCursor() {
        savedCursorColumn = cursorColumn;
        savedCursorRow = cursorRow;
    }

    public synchronized void restoreCursor() {
        cursorColumn = clamp(savedCursorColumn, 0, columns - 1);
        cursorRow = clamp(savedCursorRow, 0, rows - 1);
        wrapPending = false;
        generation++;
    }

    public synchronized void setScrollRegion(int oneBasedTop, int oneBasedBottom) {
        int top = clamp(Math.max(1, oneBasedTop) - 1, 0, rows - 1);
        int bottom = oneBasedBottom <= 0
                ? rows - 1
                : clamp(oneBasedBottom - 1, 0, rows - 1);
        if (top >= bottom) {
            scrollTop = 0;
            scrollBottom = rows - 1;
        } else {
            scrollTop = top;
            scrollBottom = bottom;
        }
        cursorColumn = 0;
        cursorRow = 0;
        wrapPending = false;
        generation++;
    }

    public synchronized void eraseDisplay(int mode) {
        wrapPending = false;
        if (mode == 0) {
            eraseRange(screen[cursorRow], cursorColumn, columns);
            for (int row = cursorRow + 1; row < rows; row++) {
                eraseRange(screen[row], 0, columns);
            }
        } else if (mode == 1) {
            for (int row = 0; row < cursorRow; row++) {
                eraseRange(screen[row], 0, columns);
            }
            eraseRange(screen[cursorRow], 0, cursorColumn + 1);
        } else if (mode == 2) {
            for (Line line : screen) {
                eraseRange(line, 0, columns);
            }
        } else if (mode == 3) {
            scrollback.clear();
        }
        generation++;
    }

    public synchronized void eraseLine(int mode) {
        wrapPending = false;
        if (mode == 0) {
            eraseRange(screen[cursorRow], cursorColumn, columns);
        } else if (mode == 1) {
            eraseRange(screen[cursorRow], 0, cursorColumn + 1);
        } else if (mode == 2) {
            eraseRange(screen[cursorRow], 0, columns);
        }
        generation++;
    }

    public synchronized void eraseCharacters(int count) {
        eraseRange(screen[cursorRow], cursorColumn, cursorColumn + positive(count));
        generation++;
    }

    public synchronized void insertCharacters(int count) {
        Line line = screen[cursorRow];
        int amount = Math.min(positive(count), columns - cursorColumn);
        int moved = columns - cursorColumn - amount;
        if (moved > 0) {
            System.arraycopy(line.codePoints, cursorColumn, line.codePoints, cursorColumn + amount, moved);
            System.arraycopy(line.foregrounds, cursorColumn, line.foregrounds, cursorColumn + amount, moved);
            System.arraycopy(line.backgrounds, cursorColumn, line.backgrounds, cursorColumn + amount, moved);
            System.arraycopy(line.styles, cursorColumn, line.styles, cursorColumn + amount, moved);
            System.arraycopy(line.cellFlags, cursorColumn, line.cellFlags, cursorColumn + amount, moved);
        }
        eraseRange(line, cursorColumn, cursorColumn + amount);
        sanitizeWideCells(line);
        wrapPending = false;
        generation++;
    }

    public synchronized void deleteCharacters(int count) {
        Line line = screen[cursorRow];
        int amount = Math.min(positive(count), columns - cursorColumn);
        int moved = columns - cursorColumn - amount;
        if (moved > 0) {
            System.arraycopy(line.codePoints, cursorColumn + amount, line.codePoints, cursorColumn, moved);
            System.arraycopy(line.foregrounds, cursorColumn + amount, line.foregrounds, cursorColumn, moved);
            System.arraycopy(line.backgrounds, cursorColumn + amount, line.backgrounds, cursorColumn, moved);
            System.arraycopy(line.styles, cursorColumn + amount, line.styles, cursorColumn, moved);
            System.arraycopy(line.cellFlags, cursorColumn + amount, line.cellFlags, cursorColumn, moved);
        }
        eraseRange(line, columns - amount, columns);
        sanitizeWideCells(line);
        wrapPending = false;
        generation++;
    }

    public synchronized void insertLines(int count) {
        if (cursorRow < scrollTop || cursorRow > scrollBottom) {
            return;
        }
        scrollDownInternal(cursorRow, scrollBottom, positive(count));
        generation++;
    }

    public synchronized void deleteLines(int count) {
        if (cursorRow < scrollTop || cursorRow > scrollBottom) {
            return;
        }
        scrollUpInternal(cursorRow, scrollBottom, positive(count));
        generation++;
    }

    public synchronized void scrollUp(int count) {
        scrollUpInternal(scrollTop, scrollBottom, positive(count));
        generation++;
    }

    public synchronized void scrollDown(int count) {
        scrollDownInternal(scrollTop, scrollBottom, positive(count));
        generation++;
    }

    public synchronized void applySgr(int[] parameters) {
        if (parameters == null || parameters.length == 0) {
            resetAttributes();
            generation++;
            return;
        }
        for (int i = 0; i < parameters.length; i++) {
            int parameter = parameters[i] < 0 ? 0 : parameters[i];
            switch (parameter) {
                case 0:
                    resetAttributes();
                    break;
                case 1:
                    currentStyle |= STYLE_BOLD;
                    break;
                case 2:
                    currentStyle |= STYLE_FAINT;
                    break;
                case 3:
                    currentStyle |= STYLE_ITALIC;
                    break;
                case 4:
                case 21:
                    currentStyle |= STYLE_UNDERLINE;
                    break;
                case 5:
                case 6:
                    currentStyle |= STYLE_BLINK;
                    break;
                case 7:
                    currentStyle |= STYLE_INVERSE;
                    break;
                case 8:
                    currentStyle |= STYLE_INVISIBLE;
                    break;
                case 9:
                    currentStyle |= STYLE_STRIKE;
                    break;
                case 22:
                    currentStyle &= ~(STYLE_BOLD | STYLE_FAINT);
                    break;
                case 23:
                    currentStyle &= ~STYLE_ITALIC;
                    break;
                case 24:
                    currentStyle &= ~STYLE_UNDERLINE;
                    break;
                case 25:
                    currentStyle &= ~STYLE_BLINK;
                    break;
                case 27:
                    currentStyle &= ~STYLE_INVERSE;
                    break;
                case 28:
                    currentStyle &= ~STYLE_INVISIBLE;
                    break;
                case 29:
                    currentStyle &= ~STYLE_STRIKE;
                    break;
                case 39:
                    currentForeground = COLOR_DEFAULT;
                    break;
                case 49:
                    currentBackground = COLOR_DEFAULT;
                    break;
                default:
                    if (parameter >= 30 && parameter <= 37) {
                        currentForeground = indexedColor(parameter - 30);
                    } else if (parameter >= 40 && parameter <= 47) {
                        currentBackground = indexedColor(parameter - 40);
                    } else if (parameter >= 90 && parameter <= 97) {
                        currentForeground = indexedColor(parameter - 90 + 8);
                    } else if (parameter >= 100 && parameter <= 107) {
                        currentBackground = indexedColor(parameter - 100 + 8);
                    } else if (parameter == 38 || parameter == 48) {
                        boolean foreground = parameter == 38;
                        int modeIndex = nextPresent(parameters, i + 1);
                        if (modeIndex >= 0 && parameters[modeIndex] == 5) {
                            int valueIndex = nextPresent(parameters, modeIndex + 1);
                            if (valueIndex >= 0) {
                                setCurrentColor(foreground, indexedColor(parameters[valueIndex]));
                                i = valueIndex;
                            }
                        } else if (modeIndex >= 0 && parameters[modeIndex] == 2) {
                            int redIndex = nextPresent(parameters, modeIndex + 1);
                            int greenIndex = redIndex < 0 ? -1 : nextPresent(parameters, redIndex + 1);
                            int blueIndex = greenIndex < 0 ? -1 : nextPresent(parameters, greenIndex + 1);
                            if (blueIndex >= 0) {
                                setCurrentColor(
                                        foreground,
                                        rgbColor(
                                                parameters[redIndex],
                                                parameters[greenIndex],
                                                parameters[blueIndex]));
                                i = blueIndex;
                            }
                        }
                    }
                    break;
            }
        }
        generation++;
    }

    public synchronized void reset() {
        primaryScreen = null;
        alternateScreen = false;
        scrollback = new ArrayDeque<>();
        resetAttributes();
        cursorColumn = 0;
        cursorRow = 0;
        savedCursorColumn = 0;
        savedCursorRow = 0;
        scrollTop = 0;
        scrollBottom = rows - 1;
        wrapPending = false;
        autoWrap = true;
        cursorVisible = true;
        cursorStyle = CursorStyle.BLOCK;
        for (int row = 0; row < rows; row++) {
            screen[row] = blankLine();
        }
        generation++;
    }

    private ScreenState captureScreen() {
        ScreenState state = new ScreenState();
        state.columns = columns;
        state.rows = rows;
        state.screen = screen;
        state.scrollback = scrollback;
        state.cursorColumn = cursorColumn;
        state.cursorRow = cursorRow;
        state.savedCursorColumn = savedCursorColumn;
        state.savedCursorRow = savedCursorRow;
        state.scrollTop = scrollTop;
        state.scrollBottom = scrollBottom;
        state.wrapPending = wrapPending;
        state.autoWrap = autoWrap;
        state.cursorVisible = cursorVisible;
        state.cursorStyle = cursorStyle;
        state.currentForeground = currentForeground;
        state.currentBackground = currentBackground;
        state.currentStyle = currentStyle;
        return state;
    }

    private void restoreScreen(ScreenState state) {
        columns = state.columns;
        rows = state.rows;
        screen = state.screen;
        scrollback = state.scrollback;
        cursorColumn = state.cursorColumn;
        cursorRow = state.cursorRow;
        savedCursorColumn = state.savedCursorColumn;
        savedCursorRow = state.savedCursorRow;
        scrollTop = state.scrollTop;
        scrollBottom = state.scrollBottom;
        wrapPending = state.wrapPending;
        autoWrap = state.autoWrap;
        cursorVisible = state.cursorVisible;
        cursorStyle = state.cursorStyle;
        currentForeground = state.currentForeground;
        currentBackground = state.currentBackground;
        currentStyle = state.currentStyle;
    }

    private void setCurrentColor(boolean foreground, int color) {
        if (foreground) {
            currentForeground = color;
        } else {
            currentBackground = color;
        }
    }

    private void resetAttributes() {
        currentForeground = COLOR_DEFAULT;
        currentBackground = COLOR_DEFAULT;
        currentStyle = 0;
    }

    private void lineFeedInternal() {
        if (cursorRow == scrollBottom) {
            scrollUpInternal(scrollTop, scrollBottom, 1);
        } else {
            cursorRow = Math.min(rows - 1, cursorRow + 1);
        }
    }

    private void scrollUpInternal(int top, int bottom, int requestedCount) {
        int count = Math.min(requestedCount, bottom - top + 1);
        for (int ignored = 0; ignored < count; ignored++) {
            Line removed = screen[top];
            if (top == 0 && bottom == rows - 1) {
                addScrollback(removed);
            }
            if (bottom - top >= 0) {
                System.arraycopy(screen, top + 1, screen, top, bottom - top);
            }
            screen[bottom] = blankLine();
        }
    }

    private void scrollDownInternal(int top, int bottom, int requestedCount) {
        int count = Math.min(requestedCount, bottom - top + 1);
        for (int ignored = 0; ignored < count; ignored++) {
            if (bottom - top >= 0) {
                System.arraycopy(screen, top, screen, top + 1, bottom - top);
            }
            screen[top] = blankLine();
        }
    }

    private void addScrollback(Line line) {
        if (alternateScreen || scrollbackLimit <= 0) {
            return;
        }
        scrollback.addLast(line);
        trimScrollback();
    }

    private void trimScrollback() {
        trimDeque(scrollback);
    }

    private void trimDeque(ArrayDeque<Line> lines) {
        while (lines.size() > scrollbackLimit) {
            lines.removeFirst();
        }
    }

    private Line blankLine() {
        return new Line(columns, currentForeground, currentBackground);
    }

    private Line resizeLine(Line source, int newColumns) {
        Line resized = new Line(newColumns, COLOR_DEFAULT, COLOR_DEFAULT);
        int copy = Math.min(source.codePoints.length, newColumns);
        System.arraycopy(source.codePoints, 0, resized.codePoints, 0, copy);
        System.arraycopy(source.foregrounds, 0, resized.foregrounds, 0, copy);
        System.arraycopy(source.backgrounds, 0, resized.backgrounds, 0, copy);
        System.arraycopy(source.styles, 0, resized.styles, 0, copy);
        System.arraycopy(source.cellFlags, 0, resized.cellFlags, 0, copy);
        resized.wrapped = source.wrapped;
        sanitizeWideCells(resized);
        return resized;
    }

    private void copyLineToSnapshot(
            Line source,
            int[] destinationCodePoints,
            int[] destinationForegrounds,
            int[] destinationBackgrounds,
            byte[] destinationStyles,
            byte[] destinationCellFlags) {
        int count = Math.min(columns, source.codePoints.length);
        System.arraycopy(source.codePoints, 0, destinationCodePoints, 0, count);
        System.arraycopy(source.foregrounds, 0, destinationForegrounds, 0, count);
        System.arraycopy(source.backgrounds, 0, destinationBackgrounds, 0, count);
        System.arraycopy(source.styles, 0, destinationStyles, 0, count);
        System.arraycopy(source.cellFlags, 0, destinationCellFlags, 0, count);
    }

    private Line documentLineAt(int documentLine, Line[] history) {
        return documentLine < history.length
                ? history[documentLine]
                : screen[documentLine - history.length];
    }

    private static int codePointAt(Line line, int column) {
        return column >= 0 && column < line.codePoints.length ? line.codePoints[column] : 0;
    }

    private static boolean isContinuation(Line line, int column) {
        return column >= 0
                && column < line.cellFlags.length
                && (line.cellFlags[column] & CELL_WIDE_CONTINUATION) != 0;
    }

    private static boolean isWideCell(Line line, int column) {
        return column + 1 < line.cellFlags.length && isContinuation(line, column + 1);
    }

    private static int selectionCharacterClass(int codePoint) {
        if (codePoint == 0 || Character.isWhitespace(codePoint)) {
            return 0;
        }
        if (Character.isLetterOrDigit(codePoint)
                || codePoint == '_'
                || codePoint == '-'
                || codePoint == '.'
                || codePoint == '/'
                || codePoint == '~'
                || codePoint == ':'
                || codePoint == '@'
                || codePoint == '%'
                || codePoint == '+'
                || codePoint == '='
                || codePoint == '#'
                || codePoint == '$'
                || codePoint == '?'
                || codePoint == '&') {
            return 1;
        }
        return 2;
    }

    private static int comparePosition(
            int leftLine,
            int leftColumn,
            int rightLine,
            int rightColumn) {
        if (leftLine != rightLine) {
            return leftLine < rightLine ? -1 : 1;
        }
        return Integer.compare(leftColumn, rightColumn);
    }

    private void eraseRange(Line line, int requestedStart, int requestedEnd) {
        int start = clamp(requestedStart, 0, columns);
        int end = clamp(requestedEnd, start, columns);
        for (int column = start; column < end; column++) {
            clearWidePairAt(line, column);
            line.codePoints[column] = 0;
            line.foregrounds[column] = currentForeground;
            line.backgrounds[column] = currentBackground;
            line.styles[column] = 0;
            line.cellFlags[column] = 0;
        }
        line.wrapped = false;
    }

    private void clearWidePairAt(int row, int column) {
        clearWidePairAt(screen[row], column);
    }

    private void clearWidePairAt(Line line, int column) {
        if (column < 0 || column >= line.codePoints.length) {
            return;
        }
        if ((line.cellFlags[column] & CELL_WIDE_CONTINUATION) != 0) {
            clearCell(line, column);
            if (column > 0) {
                clearCell(line, column - 1);
            }
        } else if (column + 1 < line.codePoints.length
                && (line.cellFlags[column + 1] & CELL_WIDE_CONTINUATION) != 0) {
            clearCell(line, column);
            clearCell(line, column + 1);
        }
    }

    private void clearCell(Line line, int column) {
        line.codePoints[column] = 0;
        line.foregrounds[column] = currentForeground;
        line.backgrounds[column] = currentBackground;
        line.styles[column] = 0;
        line.cellFlags[column] = 0;
    }

    private static void sanitizeWideCells(Line line) {
        for (int column = 0; column < line.codePoints.length; column++) {
            if ((line.cellFlags[column] & CELL_WIDE_CONTINUATION) != 0) {
                if (column == 0 || !isWide(line.codePoints[column - 1])) {
                    line.cellFlags[column] = 0;
                }
            } else if (isWide(line.codePoints[column])) {
                if (column + 1 >= line.codePoints.length
                        || (line.cellFlags[column + 1] & CELL_WIDE_CONTINUATION) == 0) {
                    line.codePoints[column] = 0;
                }
            }
        }
    }

    private static int nextPresent(int[] parameters, int start) {
        for (int index = start; index < parameters.length; index++) {
            if (parameters[index] >= 0) {
                return index;
            }
        }
        return -1;
    }

    private static boolean isControl(int codePoint) {
        return codePoint < 0x20 || (codePoint >= 0x7f && codePoint < 0xa0);
    }

    private static boolean isCombining(int codePoint) {
        int type = Character.getType(codePoint);
        return type == Character.NON_SPACING_MARK
                || type == Character.COMBINING_SPACING_MARK
                || type == Character.ENCLOSING_MARK;
    }

    // Compact wcwidth approximation covering the common CJK and emoji ranges used by terminals.
    private static boolean isWide(int codePoint) {
        return codePoint >= 0x1100
                && (codePoint <= 0x115f
                        || codePoint == 0x2329
                        || codePoint == 0x232a
                        || (codePoint >= 0x2e80 && codePoint <= 0xa4cf && codePoint != 0x303f)
                        || (codePoint >= 0xac00 && codePoint <= 0xd7a3)
                        || (codePoint >= 0xf900 && codePoint <= 0xfaff)
                        || (codePoint >= 0xfe10 && codePoint <= 0xfe19)
                        || (codePoint >= 0xfe30 && codePoint <= 0xfe6f)
                        || (codePoint >= 0xff00 && codePoint <= 0xff60)
                        || (codePoint >= 0xffe0 && codePoint <= 0xffe6)
                        || (codePoint >= 0x1f300 && codePoint <= 0x1faff)
                        || (codePoint >= 0x20000 && codePoint <= 0x3fffd));
    }

    private static int positive(int value) {
        return value <= 0 ? 1 : value;
    }

    private static int clamp(int value, int minimum, int maximum) {
        return Math.max(minimum, Math.min(maximum, value));
    }
}
