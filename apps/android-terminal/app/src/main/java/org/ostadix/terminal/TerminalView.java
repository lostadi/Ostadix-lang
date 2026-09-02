package org.ostadix.terminal;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.graphics.Rect;
import android.graphics.Typeface;
import android.os.Looper;
import android.text.Editable;
import android.text.InputType;
import android.text.SpannableStringBuilder;
import android.util.AttributeSet;
import android.util.TypedValue;
import android.view.ActionMode;
import android.view.GestureDetector;
import android.view.HapticFeedbackConstants;
import android.view.KeyEvent;
import android.view.Menu;
import android.view.MenuItem;
import android.view.MotionEvent;
import android.view.View;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;
import android.widget.OverScroller;

import java.nio.charset.StandardCharsets;
import java.util.Arrays;

/**
 * Dependency-free terminal renderer and Android input surface.
 *
 * <p>PTY ownership intentionally stays outside this class. Feed immutable output chunks through
 * {@link #feed(byte[])} and forward {@link InputListener} bytes to the active PTY session.</p>
 */
public final class TerminalView extends View {
    private static final int DEFAULT_COLUMNS = 80;
    private static final int DEFAULT_ROWS = 24;
    private static final int DEFAULT_SCROLLBACK = 5_000;
    private static final float DEFAULT_FONT_SIZE_SP = 14f;
    private static final float MIN_FONT_SIZE_SP = 8f;
    private static final float MAX_FONT_SIZE_SP = 48f;
    private static final long CURSOR_BLINK_MILLIS = 500L;
    private static final int MAX_IME_DELETE = 1_024;
    private static final int MENU_COPY = 1;
    private static final int MENU_SELECT_ALL = 2;
    private static final int MENU_CLEAR = 3;
    private static final int HANDLE_NONE = 0;
    private static final int HANDLE_START = 1;
    private static final int HANDLE_END = 2;
    private static final int HANDLE_DYNAMIC = 3;

    public interface InputListener {
        void onTerminalInput(byte[] data);
    }

    public interface ResizeListener {
        void onTerminalResize(int columns, int rows);
    }

    public interface TitleListener {
        void onTerminalTitleChanged(String title);
    }

    public interface BellListener {
        void onTerminalBell();
    }

    public enum SpecialKey {
        ESCAPE,
        TAB,
        ENTER,
        BACKSPACE,
        DELETE,
        INSERT,
        UP,
        DOWN,
        LEFT,
        RIGHT,
        HOME,
        END,
        PAGE_UP,
        PAGE_DOWN,
        F1,
        F2,
        F3,
        F4,
        F5,
        F6,
        F7,
        F8,
        F9,
        F10,
        F11,
        F12
    }

    private static final class CellAddress {
        final int documentLine;
        final int column;
        final int trailingColumn;

        CellAddress(int documentLine, int column, int trailingColumn) {
            this.documentLine = documentLine;
            this.column = column;
            this.trailingColumn = trailingColumn;
        }
    }

    private final TerminalBuffer buffer;
    private final AnsiParser parser;
    private final Paint textPaint = new Paint(Paint.ANTI_ALIAS_FLAG | Paint.SUBPIXEL_TEXT_FLAG);
    private final Paint fillPaint = new Paint();
    private final Paint decorationPaint = new Paint(Paint.ANTI_ALIAS_FLAG);
    private final GestureDetector gestureDetector;
    private final OverScroller scroller;
    private final Editable imeEditable = new SpannableStringBuilder();

    private volatile InputListener inputListener;
    private volatile ResizeListener resizeListener;
    private volatile TitleListener titleListener;
    private volatile BellListener bellListener;

    private Typeface typeface = Typeface.MONOSPACE;
    private float fontSizeSp = DEFAULT_FONT_SIZE_SP;
    private float cellWidth;
    private float cellHeight;
    private float baselineOffset;
    private int scrollOffsetPixels;
    private boolean virtualControl;
    private boolean virtualAlt;
    private boolean cursorBlinkEnabled = true;
    private volatile boolean terminalResponsesEnabled = true;
    private boolean blinkPhase = true;
    private boolean attached;
    private String composingText = "";

    private boolean selectionActive;
    private int selectionStartLine;
    private int selectionStartColumn;
    private int selectionEndLine;
    private int selectionEndColumn;
    private int selectionBaseStartLine;
    private int selectionBaseStartColumn;
    private int selectionBaseEndLine;
    private int selectionBaseEndColumn;
    private int activeSelectionHandle = HANDLE_NONE;
    private boolean selectionDragging;
    private int selectionHighlightColor = 0xff263445;
    private ActionMode selectionActionMode;

    private final Runnable cursorBlink = new Runnable() {
        @Override
        public void run() {
            if (!attached || !cursorBlinkEnabled) {
                return;
            }
            blinkPhase = !blinkPhase;
            invalidate();
            postDelayed(this, CURSOR_BLINK_MILLIS);
        }
    };

    private final ActionMode.Callback2 selectionActionCallback = new ActionMode.Callback2() {
        @Override
        public boolean onCreateActionMode(ActionMode mode, Menu menu) {
            menu.add(Menu.NONE, MENU_COPY, 0, "Copy")
                    .setShowAsAction(MenuItem.SHOW_AS_ACTION_IF_ROOM);
            menu.add(Menu.NONE, MENU_SELECT_ALL, 1, "Select all")
                    .setShowAsAction(MenuItem.SHOW_AS_ACTION_IF_ROOM);
            menu.add(Menu.NONE, MENU_CLEAR, 2, "Clear");
            return true;
        }

        @Override
        public boolean onPrepareActionMode(ActionMode mode, Menu menu) {
            MenuItem copy = menu.findItem(MENU_COPY);
            if (copy != null) {
                copy.setEnabled(selectionActive);
            }
            return true;
        }

        @Override
        public boolean onActionItemClicked(ActionMode mode, MenuItem item) {
            if (item.getItemId() == MENU_COPY) {
                copySelectionToClipboard();
                mode.finish();
                return true;
            }
            if (item.getItemId() == MENU_SELECT_ALL) {
                selectAll();
                return true;
            }
            if (item.getItemId() == MENU_CLEAR) {
                mode.finish();
                return true;
            }
            return false;
        }

        @Override
        public void onDestroyActionMode(ActionMode mode) {
            if (selectionActionMode == mode) {
                selectionActionMode = null;
            }
            clearSelectionState();
        }

        @Override
        public void onGetContentRect(ActionMode mode, View view, Rect outRect) {
            selectionContentRect(outRect);
        }
    };

    public TerminalView(Context context) {
        this(context, null);
    }

    public TerminalView(Context context, AttributeSet attrs) {
        this(context, attrs, 0);
    }

    public TerminalView(Context context, AttributeSet attrs, int defStyleAttr) {
        super(context, attrs, defStyleAttr);
        buffer = new TerminalBuffer(DEFAULT_COLUMNS, DEFAULT_ROWS, DEFAULT_SCROLLBACK);
        parser = new AnsiParser(buffer);
        scroller = new OverScroller(context);
        gestureDetector = new GestureDetector(context, new TerminalGestureListener());

        textPaint.setTypeface(typeface);
        textPaint.setLinearText(true);
        decorationPaint.setStrokeWidth(dp(1f));
        updateFontMetrics();
        if (getPaddingLeft() == 0 && getPaddingTop() == 0
                && getPaddingRight() == 0 && getPaddingBottom() == 0) {
            int padding = Math.round(dp(8f));
            setPadding(padding, padding, padding, padding);
        }

        setFocusable(true);
        setFocusableInTouchMode(true);
        setClickable(true);
        setLongClickable(true);
        setVerticalScrollBarEnabled(false);

        parser.setTitleListener(new AnsiParser.TitleListener() {
            @Override
            public void onTitleChanged(final String title) {
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        TitleListener listener = titleListener;
                        if (listener != null) {
                            listener.onTerminalTitleChanged(title);
                        }
                    }
                });
            }
        });
        parser.setBellListener(new AnsiParser.BellListener() {
            @Override
            public void onBell() {
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        BellListener listener = bellListener;
                        if (listener != null) {
                            listener.onTerminalBell();
                        }
                    }
                });
            }
        });
        parser.setResponseListener(new AnsiParser.ResponseListener() {
            @Override
            public void onResponse(final byte[] response) {
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        if (terminalResponsesEnabled) {
                            dispatchInput(response);
                        }
                    }
                });
            }
        });
    }

    public TerminalBuffer getBuffer() {
        return buffer;
    }

    public int getColumns() {
        return buffer.getColumns();
    }

    public int getRows() {
        return buffer.getRows();
    }

    public float getFontSizeSp() {
        return fontSizeSp;
    }

    public void setInputListener(InputListener listener) {
        inputListener = listener;
    }

    public void setResizeListener(ResizeListener listener) {
        resizeListener = listener;
    }

    public void setTitleListener(TitleListener listener) {
        titleListener = listener;
    }

    public void setBellListener(BellListener listener) {
        bellListener = listener;
    }

    public boolean hasSelection() {
        return selectionActive;
    }

    public String getSelectedText() {
        if (!selectionActive) {
            return "";
        }
        return buffer.extractText(
                selectionStartLine,
                selectionStartColumn,
                selectionEndLine,
                selectionEndColumn);
    }

    /** Copies the current selection as plain text without sending anything to the PTY. */
    public boolean copySelectionToClipboard() {
        String text = getSelectedText();
        if (text.isEmpty()) {
            return false;
        }
        ClipboardManager clipboard = (ClipboardManager)
                getContext().getSystemService(Context.CLIPBOARD_SERVICE);
        if (clipboard == null) {
            return false;
        }
        clipboard.setPrimaryClip(ClipData.newPlainText("Terminal text", text));
        performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP);
        return true;
    }

    public void selectAll() {
        int documentLines = buffer.getDocumentLineCount();
        if (documentLines <= 0) {
            return;
        }
        selectionStartLine = 0;
        selectionStartColumn = 0;
        selectionEndLine = documentLines - 1;
        selectionEndColumn = Math.max(0, buffer.getColumns() - 1);
        selectionActive = true;
        activeSelectionHandle = HANDLE_NONE;
        selectionDragging = false;
        ensureSelectionActionMode();
        invalidateSelectionActionMode();
        invalidate();
    }

    public void clearSelection() {
        if (Looper.myLooper() != Looper.getMainLooper()) {
            post(new Runnable() {
                @Override
                public void run() {
                    clearSelection();
                }
            });
            return;
        }
        ActionMode mode = selectionActionMode;
        if (mode != null) {
            mode.finish();
        } else {
            clearSelectionState();
        }
    }

    public void setSelectionColor(int color) {
        selectionHighlightColor = 0xff000000 | (color & 0x00ffffff);
        invalidate();
    }

    /** Enables fixed DA/DSR replies while the view is attached to a real PTY. */
    public void setTerminalResponsesEnabled(boolean enabled) {
        terminalResponsesEnabled = enabled;
    }

    public void feed(byte[] bytes) {
        if (bytes == null) {
            throw new IllegalArgumentException("bytes must not be null");
        }
        feed(bytes, 0, bytes.length);
    }

    public void feed(byte[] bytes, int offset, int length) {
        int historyBefore = buffer.getScrollbackSize();
        parser.feed(bytes, offset, length);
        refreshAfterOutput(historyBefore);
    }

    public void finishOutput() {
        int historyBefore = buffer.getScrollbackSize();
        parser.finish();
        refreshAfterOutput(historyBefore);
    }

    public void resetTerminal() {
        parser.reset();
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                clearSelection();
                scroller.abortAnimation();
                scrollOffsetPixels = 0;
                composingText = "";
                restartCursorBlink();
                invalidate();
            }
        });
    }

    /** Sends ordinary terminal input after applying the virtual Ctrl/Alt modifier state. */
    public void sendInput(String text) {
        if (text == null || text.isEmpty()) {
            return;
        }
        // Extra-key rows commonly pass complete VT sequences through this overload. Applying Alt
        // to every byte of an ESC-prefixed sequence would corrupt it, so control-bearing strings
        // are always treated as exact terminal input.
        if (containsControlCharacter(text)) {
            dispatchInput(text.getBytes(StandardCharsets.UTF_8));
            return;
        }
        sendTextWithModifiers(text, virtualControl, virtualAlt);
    }

    /** Sends exact bytes without transforming them. Useful for an app-owned extra-key row. */
    public void sendInput(byte[] bytes) {
        if (bytes == null) {
            throw new IllegalArgumentException("bytes must not be null");
        }
        dispatchInput(bytes.clone());
    }

    public void sendSpecialKey(SpecialKey key) {
        if (key == null) {
            return;
        }
        sendSpecialKey(key, false, virtualAlt, virtualControl);
    }

    public void setVirtualModifiers(boolean control, boolean alt) {
        virtualControl = control;
        virtualAlt = alt;
    }

    public boolean isVirtualControlEnabled() {
        return virtualControl;
    }

    public boolean isVirtualAltEnabled() {
        return virtualAlt;
    }

    public void setFontSizeSp(float sizeSp) {
        float safe = Math.max(MIN_FONT_SIZE_SP, Math.min(MAX_FONT_SIZE_SP, sizeSp));
        if (Math.abs(safe - fontSizeSp) < 0.01f) {
            return;
        }
        fontSizeSp = safe;
        updateFontMetrics();
        requestLayout();
        updateTerminalDimensions(getWidth(), getHeight());
        invalidate();
    }

    public void setTypeface(Typeface typeface) {
        this.typeface = typeface == null ? Typeface.MONOSPACE : typeface;
        textPaint.setTypeface(this.typeface);
        updateFontMetrics();
        requestLayout();
        updateTerminalDimensions(getWidth(), getHeight());
        invalidate();
    }

    public void setPalette(TerminalBuffer.Palette palette) {
        buffer.setPalette(palette);
        invalidate();
    }

    public void setCursorStyle(TerminalBuffer.CursorStyle style) {
        buffer.setCursorStyle(style);
        restartCursorBlink();
        invalidate();
    }

    public void setCursorBlinkEnabled(boolean enabled) {
        cursorBlinkEnabled = enabled;
        blinkPhase = true;
        removeCallbacks(cursorBlink);
        if (enabled && attached) {
            postDelayed(cursorBlink, CURSOR_BLINK_MILLIS);
        }
        invalidate();
    }

    public void setScrollbackLimit(int lines) {
        clearSelection();
        buffer.setScrollbackLimit(lines);
        clampScrollOffset();
        invalidate();
    }

    /** Applies the app's persisted appearance settings in one redraw. */
    public void applyAppearance(
            AppPreferences.ThemePalette palette,
            float sizeSp,
            String cursorStyle,
            int scrollbackLines) {
        if (palette == null) {
            throw new IllegalArgumentException("palette must not be null");
        }
        setPalette(new TerminalBuffer.Palette(
                palette.ansiColors(),
                palette.foreground,
                palette.background,
                palette.cursor));
        setSelectionColor(palette.selection);
        setScrollbackLimit(scrollbackLines);
        setFontSizeSp(sizeSp);
        if (AppPreferences.CURSOR_BAR.equals(cursorStyle)) {
            setCursorStyle(TerminalBuffer.CursorStyle.BAR);
        } else if (AppPreferences.CURSOR_UNDERLINE.equals(cursorStyle)) {
            setCursorStyle(TerminalBuffer.CursorStyle.UNDERLINE);
        } else {
            setCursorStyle(TerminalBuffer.CursorStyle.BLOCK);
        }
    }

    public void scrollToBottom() {
        scroller.abortAnimation();
        scrollOffsetPixels = 0;
        invalidate();
    }

    public void scrollByLines(int lines) {
        setScrollOffsetPixels(scrollOffsetPixels + Math.round(lines * cellHeight));
    }

    public void showSoftKeyboard() {
        requestFocus();
        InputMethodManager manager = (InputMethodManager)
                getContext().getSystemService(Context.INPUT_METHOD_SERVICE);
        if (manager != null) {
            manager.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT);
        }
    }

    @Override
    public boolean onCheckIsTextEditor() {
        return true;
    }

    @Override
    public InputConnection onCreateInputConnection(EditorInfo outAttrs) {
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT
                | InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD
                | InputType.TYPE_TEXT_FLAG_MULTI_LINE
                | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS;
        outAttrs.imeOptions = EditorInfo.IME_ACTION_NONE
                | EditorInfo.IME_FLAG_NO_EXTRACT_UI
                | EditorInfo.IME_FLAG_NO_FULLSCREEN;
        outAttrs.initialSelStart = 0;
        outAttrs.initialSelEnd = 0;

        return new BaseInputConnection(this, false) {
            @Override
            public Editable getEditable() {
                return imeEditable;
            }

            @Override
            public boolean commitText(CharSequence text, int newCursorPosition) {
                applyComposingText(text == null ? "" : text.toString(), true);
                return true;
            }

            @Override
            public boolean setComposingText(CharSequence text, int newCursorPosition) {
                applyComposingText(text == null ? "" : text.toString(), false);
                return true;
            }

            @Override
            public boolean finishComposingText() {
                composingText = "";
                imeEditable.clear();
                return true;
            }

            @Override
            public boolean deleteSurroundingText(int beforeLength, int afterLength) {
                composingText = "";
                imeEditable.clear();
                int backwards = Math.min(MAX_IME_DELETE, Math.max(0, beforeLength));
                if (backwards > 0) {
                    byte[] deletes = new byte[backwards];
                    Arrays.fill(deletes, (byte) 0x7f);
                    dispatchInput(deletes);
                }
                int forwards = Math.min(MAX_IME_DELETE, Math.max(0, afterLength));
                for (int index = 0; index < forwards; index++) {
                    dispatchInput(new byte[] {0x1b, '[', '3', '~'});
                }
                return true;
            }

            @Override
            public boolean sendKeyEvent(KeyEvent event) {
                return TerminalView.this.dispatchKeyEvent(event);
            }

            @Override
            public boolean performEditorAction(int actionCode) {
                sendSpecialKey(SpecialKey.ENTER);
                return true;
            }
        };
    }

    @Override
    public boolean onKeyDown(int keyCode, KeyEvent event) {
        if (selectionActive) {
            if (keyCode == KeyEvent.KEYCODE_C && event.isCtrlPressed()) {
                copySelectionToClipboard();
                clearSelection();
                return true;
            }
            if (keyCode == KeyEvent.KEYCODE_ESCAPE) {
                clearSelection();
                return true;
            }
        }
        SpecialKey specialKey = specialKeyForAndroidCode(keyCode);
        boolean control = event.isCtrlPressed() || virtualControl;
        boolean alt = event.isAltPressed() || virtualAlt;
        boolean shift = event.isShiftPressed();
        if (specialKey != null) {
            sendSpecialKey(specialKey, shift, alt, control);
            return true;
        }

        int metaState = event.getMetaState()
                & ~(KeyEvent.META_CTRL_MASK | KeyEvent.META_ALT_MASK | KeyEvent.META_META_MASK);
        int unicode = event.getUnicodeChar(metaState);
        if (unicode != 0) {
            sendCodePointWithModifiers(unicode, control, alt);
            return true;
        }
        return super.onKeyDown(keyCode, event);
    }

    @Override
    public boolean onKeyMultiple(int keyCode, int repeatCount, KeyEvent event) {
        if (keyCode == KeyEvent.KEYCODE_UNKNOWN && event.getCharacters() != null) {
            sendTextWithModifiers(event.getCharacters(), virtualControl, virtualAlt);
            return true;
        }
        return super.onKeyMultiple(keyCode, repeatCount, event);
    }

    @Override
    protected void onSizeChanged(int width, int height, int oldWidth, int oldHeight) {
        super.onSizeChanged(width, height, oldWidth, oldHeight);
        updateTerminalDimensions(width, height);
    }

    @Override
    protected void onDraw(Canvas canvas) {
        super.onDraw(canvas);
        int offsetLines = cellHeight <= 0f ? 0 : Math.round(scrollOffsetPixels / cellHeight);
        TerminalBuffer.Snapshot snapshot = buffer.snapshot(offsetLines);
        canvas.drawColor(snapshot.defaultBackground());
        canvas.save();
        canvas.clipRect(
                getPaddingLeft(),
                getPaddingTop(),
                getWidth() - getPaddingRight(),
                getHeight() - getPaddingBottom());

        drawCellBackgrounds(canvas, snapshot);
        drawSelection(canvas, snapshot);
        drawCellGlyphs(canvas, snapshot);
        drawCursor(canvas, snapshot);
        drawSelectionHandles(canvas, snapshot);
        canvas.restore();
    }

    @Override
    public boolean onTouchEvent(MotionEvent event) {
        int action = event.getActionMasked();
        if (action == MotionEvent.ACTION_DOWN && selectionActive) {
            int handle = selectionHandleNear(event.getX(), event.getY());
            if (handle != HANDLE_NONE) {
                scroller.abortAnimation();
                activeSelectionHandle = handle;
                selectionDragging = true;
                requestFocus();
                if (getParent() != null) {
                    getParent().requestDisallowInterceptTouchEvent(true);
                }
                return true;
            }
        }
        if (selectionDragging) {
            if (action == MotionEvent.ACTION_MOVE) {
                updateSelectionDrag(event.getX(), event.getY());
                return true;
            }
            if (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL) {
                updateSelectionDrag(event.getX(), event.getY());
                selectionDragging = false;
                activeSelectionHandle = HANDLE_NONE;
                invalidateSelectionActionMode();
                invalidate();
                return true;
            }
        }
        boolean handled = gestureDetector.onTouchEvent(event);
        if (action == MotionEvent.ACTION_CANCEL) {
            scroller.abortAnimation();
        }
        return handled || super.onTouchEvent(event);
    }

    @Override
    public boolean performClick() {
        super.performClick();
        showSoftKeyboard();
        return true;
    }

    @Override
    public void computeScroll() {
        super.computeScroll();
        if (scroller.computeScrollOffset()) {
            setScrollOffsetPixels(scroller.getCurrY());
            postInvalidateOnAnimation();
        }
    }

    @Override
    protected void onAttachedToWindow() {
        super.onAttachedToWindow();
        attached = true;
        restartCursorBlink();
    }

    @Override
    protected void onDetachedFromWindow() {
        attached = false;
        clearSelection();
        removeCallbacks(cursorBlink);
        scroller.abortAnimation();
        super.onDetachedFromWindow();
    }

    @Override
    protected void onFocusChanged(
            boolean gainFocus,
            int direction,
            Rect previouslyFocusedRect) {
        super.onFocusChanged(gainFocus, direction, previouslyFocusedRect);
        blinkPhase = true;
        invalidate();
    }

    private void updateFontMetrics() {
        float pixels = TypedValue.applyDimension(
                TypedValue.COMPLEX_UNIT_SP,
                fontSizeSp,
                getResources().getDisplayMetrics());
        textPaint.setTextSize(pixels);
        textPaint.setTypeface(typeface);
        Paint.FontMetrics metrics = textPaint.getFontMetrics();
        float rawHeight = metrics.descent - metrics.ascent;
        cellHeight = Math.max(1f, (float) Math.ceil(rawHeight * 1.08f));
        baselineOffset = (cellHeight - rawHeight) * 0.5f - metrics.ascent;
        cellWidth = Math.max(1f, (float) Math.ceil(textPaint.measureText("M")));
    }

    private void updateTerminalDimensions(int width, int height) {
        if (width <= 0 || height <= 0 || cellWidth <= 0f || cellHeight <= 0f) {
            return;
        }
        int availableWidth = Math.max(1, width - getPaddingLeft() - getPaddingRight());
        int availableHeight = Math.max(1, height - getPaddingTop() - getPaddingBottom());
        int columns = Math.max(1, Math.min(
                TerminalBuffer.MAX_COLUMNS,
                (int) Math.floor(availableWidth / cellWidth)));
        int rows = Math.max(1, Math.min(
                TerminalBuffer.MAX_ROWS,
                (int) Math.floor(availableHeight / cellHeight)));
        if (columns == buffer.getColumns() && rows == buffer.getRows()) {
            return;
        }
        clearSelection();
        buffer.resize(columns, rows);
        clampScrollOffset();
        ResizeListener listener = resizeListener;
        if (listener != null) {
            listener.onTerminalResize(columns, rows);
        }
    }

    private void drawCellBackgrounds(Canvas canvas, TerminalBuffer.Snapshot snapshot) {
        float left = getPaddingLeft();
        float top = getPaddingTop();
        for (int row = 0; row < snapshot.rows; row++) {
            int runColor = effectiveBackground(snapshot, row, 0);
            int runStart = 0;
            for (int column = 1; column <= snapshot.columns; column++) {
                int color = column == snapshot.columns
                        ? Integer.MIN_VALUE
                        : effectiveBackground(snapshot, row, column);
                if (color != runColor) {
                    if (runColor != snapshot.defaultBackground()) {
                        fillPaint.setColor(runColor);
                        canvas.drawRect(
                                left + runStart * cellWidth,
                                top + row * cellHeight,
                                left + column * cellWidth,
                                top + (row + 1) * cellHeight,
                                fillPaint);
                    }
                    runColor = color;
                    runStart = column;
                }
            }
        }
    }

    private void drawSelection(Canvas canvas, TerminalBuffer.Snapshot snapshot) {
        if (!selectionActive) {
            return;
        }
        int firstLine = snapshot.firstDocumentLine;
        int lastLine = firstLine + snapshot.rows - 1;
        if (selectionEndLine < firstLine || selectionStartLine > lastLine) {
            return;
        }
        fillPaint.setColor(selectionHighlightColor);
        fillPaint.setAlpha(150);
        float left = getPaddingLeft();
        float top = getPaddingTop();
        int visibleStart = Math.max(selectionStartLine, firstLine);
        int visibleEnd = Math.min(selectionEndLine, lastLine);
        for (int documentLine = visibleStart; documentLine <= visibleEnd; documentLine++) {
            int row = documentLine - firstLine;
            int startColumn = documentLine == selectionStartLine ? selectionStartColumn : 0;
            int endColumn = documentLine == selectionEndLine
                    ? selectionEndColumn
                    : snapshot.columns - 1;
            startColumn = Math.max(0, Math.min(snapshot.columns - 1, startColumn));
            endColumn = Math.max(startColumn, Math.min(snapshot.columns - 1, endColumn));
            canvas.drawRect(
                    left + startColumn * cellWidth,
                    top + row * cellHeight,
                    left + (endColumn + 1) * cellWidth,
                    top + (row + 1) * cellHeight,
                    fillPaint);
        }
        fillPaint.setAlpha(255);
    }

    private void drawSelectionHandles(Canvas canvas, TerminalBuffer.Snapshot snapshot) {
        if (!selectionActive) {
            return;
        }
        float radius = Math.max(dp(4f), Math.min(cellWidth, cellHeight) * 0.22f);
        fillPaint.setColor(selectionHighlightColor);
        fillPaint.setAlpha(255);
        drawSelectionHandle(canvas, snapshot, selectionStartLine, selectionStartColumn, false, radius);
        drawSelectionHandle(canvas, snapshot, selectionEndLine, selectionEndColumn, true, radius);
    }

    private void drawSelectionHandle(
            Canvas canvas,
            TerminalBuffer.Snapshot snapshot,
            int documentLine,
            int column,
            boolean trailing,
            float radius) {
        int row = documentLine - snapshot.firstDocumentLine;
        if (row < 0 || row >= snapshot.rows) {
            return;
        }
        float x = getPaddingLeft() + (column + (trailing ? 1 : 0)) * cellWidth;
        float cellTop = getPaddingTop() + row * cellHeight;
        float cellBottom = cellTop + cellHeight;
        canvas.drawRect(x - dp(1f), cellTop, x + dp(1f), cellBottom, fillPaint);
        canvas.drawCircle(x, cellBottom - radius, radius, fillPaint);
    }

    private void drawCellGlyphs(Canvas canvas, TerminalBuffer.Snapshot snapshot) {
        float left = getPaddingLeft();
        float top = getPaddingTop();
        for (int row = 0; row < snapshot.rows; row++) {
            float baseline = top + row * cellHeight + baselineOffset;
            for (int column = 0; column < snapshot.columns; column++) {
                if (snapshot.isWideContinuationAt(row, column)) {
                    continue;
                }
                int codePoint = snapshot.codePointAt(row, column);
                if (codePoint == 0 || !Character.isValidCodePoint(codePoint)) {
                    continue;
                }
                byte style = snapshot.styleAt(row, column);
                if ((style & TerminalBuffer.STYLE_INVISIBLE) != 0
                        || ((style & TerminalBuffer.STYLE_BLINK) != 0 && !blinkPhase)) {
                    continue;
                }
                int foreground = effectiveForeground(snapshot, row, column);
                int background = effectiveBackground(snapshot, row, column);
                textPaint.setColor(foreground);
                textPaint.setAlpha((style & TerminalBuffer.STYLE_FAINT) != 0 ? 150 : 255);
                textPaint.setFakeBoldText((style & TerminalBuffer.STYLE_BOLD) != 0);
                textPaint.setTextSkewX((style & TerminalBuffer.STYLE_ITALIC) != 0 ? -0.22f : 0f);

                String glyph = new String(Character.toChars(codePoint));
                float x = left + column * cellWidth;
                float glyphWidth = textPaint.measureText(glyph);
                float available = isWideCell(snapshot, row, column) ? cellWidth * 2f : cellWidth;
                canvas.drawText(glyph, x + Math.max(0f, (available - glyphWidth) * 0.5f), baseline, textPaint);

                decorationPaint.setColor(foreground);
                decorationPaint.setAlpha(textPaint.getAlpha());
                if ((style & TerminalBuffer.STYLE_UNDERLINE) != 0) {
                    canvas.drawRect(
                            x,
                            baseline + dp(1f),
                            x + available,
                            baseline + dp(2f),
                            decorationPaint);
                }
                if ((style & TerminalBuffer.STYLE_STRIKE) != 0) {
                    float strike = baseline - cellHeight * 0.30f;
                    canvas.drawRect(x, strike, x + available, strike + dp(1f), decorationPaint);
                }
                // Ensure one styled cell cannot leak Paint state into the next.
                textPaint.setAlpha(255);
                textPaint.setFakeBoldText(false);
                textPaint.setTextSkewX(0f);
                decorationPaint.setAlpha(255);
                decorationPaint.setColor(background);
            }
        }
    }

    private void drawCursor(Canvas canvas, TerminalBuffer.Snapshot snapshot) {
        if (selectionActive || !snapshot.cursorVisible
                || snapshot.cursorRow < 0 || snapshot.cursorColumn < 0
                || (cursorBlinkEnabled && !blinkPhase)) {
            return;
        }
        int row = snapshot.cursorRow;
        int column = snapshot.cursorColumn;
        if (snapshot.isWideContinuationAt(row, column) && column > 0) {
            column--;
        }
        float left = getPaddingLeft() + column * cellWidth;
        float top = getPaddingTop() + row * cellHeight;
        float right = left + (isWideCell(snapshot, row, column) ? cellWidth * 2f : cellWidth);
        float bottom = top + cellHeight;
        fillPaint.setColor(snapshot.cursorColor());
        fillPaint.setAlpha(hasFocus() ? 220 : 120);

        if (snapshot.cursorStyle == TerminalBuffer.CursorStyle.UNDERLINE) {
            canvas.drawRect(left, bottom - Math.max(dp(2f), cellHeight * 0.10f), right, bottom, fillPaint);
        } else if (snapshot.cursorStyle == TerminalBuffer.CursorStyle.BAR) {
            canvas.drawRect(left, top, left + Math.max(dp(2f), cellWidth * 0.10f), bottom, fillPaint);
        } else {
            canvas.drawRect(left, top, right, bottom, fillPaint);
            int codePoint = snapshot.codePointAt(row, column);
            byte style = snapshot.styleAt(row, column);
            if (codePoint != 0 && Character.isValidCodePoint(codePoint)
                    && (style & TerminalBuffer.STYLE_INVISIBLE) == 0) {
                String glyph = new String(Character.toChars(codePoint));
                textPaint.setColor(effectiveBackground(snapshot, row, column));
                textPaint.setAlpha(255);
                textPaint.setFakeBoldText((style & TerminalBuffer.STYLE_BOLD) != 0);
                textPaint.setTextSkewX((style & TerminalBuffer.STYLE_ITALIC) != 0 ? -0.22f : 0f);
                float glyphWidth = textPaint.measureText(glyph);
                canvas.drawText(
                        glyph,
                        left + Math.max(0f, (right - left - glyphWidth) * 0.5f),
                        top + baselineOffset,
                        textPaint);
                textPaint.setFakeBoldText(false);
                textPaint.setTextSkewX(0f);
            }
        }
        fillPaint.setAlpha(255);
    }

    private int effectiveForeground(TerminalBuffer.Snapshot snapshot, int row, int column) {
        byte style = snapshot.styleAt(row, column);
        return (style & TerminalBuffer.STYLE_INVERSE) != 0
                ? snapshot.backgroundAt(row, column)
                : snapshot.foregroundAt(row, column);
    }

    private int effectiveBackground(TerminalBuffer.Snapshot snapshot, int row, int column) {
        byte style = snapshot.styleAt(row, column);
        return (style & TerminalBuffer.STYLE_INVERSE) != 0
                ? snapshot.foregroundAt(row, column)
                : snapshot.backgroundAt(row, column);
    }

    private boolean isWideCell(TerminalBuffer.Snapshot snapshot, int row, int column) {
        return column + 1 < snapshot.columns && snapshot.isWideContinuationAt(row, column + 1);
    }

    private void startWordSelection(float x, float y) {
        CellAddress address = cellAddressAt(x, y);
        if (address == null) {
            return;
        }
        TerminalBuffer.WordRange word = buffer.wordRangeAt(address.documentLine, address.column);
        if (word == null) {
            return;
        }
        scroller.abortAnimation();
        selectionStartLine = word.documentLine;
        selectionStartColumn = word.startColumn;
        selectionEndLine = word.documentLine;
        selectionEndColumn = word.endColumn;
        selectionBaseStartLine = selectionStartLine;
        selectionBaseStartColumn = selectionStartColumn;
        selectionBaseEndLine = selectionEndLine;
        selectionBaseEndColumn = selectionEndColumn;
        selectionActive = true;
        selectionDragging = true;
        activeSelectionHandle = HANDLE_DYNAMIC;
        performHapticFeedback(HapticFeedbackConstants.LONG_PRESS);
        ensureSelectionActionMode();
        invalidateSelectionActionMode();
        invalidate();
    }

    private void updateSelectionDrag(float x, float y) {
        if (!selectionActive) {
            return;
        }
        float contentTop = getPaddingTop();
        float contentBottom = getHeight() - getPaddingBottom();
        if (y < contentTop + cellHeight * 0.55f) {
            setScrollOffsetPixels(scrollOffsetPixels + Math.round(cellHeight));
        } else if (y > contentBottom - cellHeight * 0.55f) {
            setScrollOffsetPixels(scrollOffsetPixels - Math.round(cellHeight));
        }
        CellAddress address = cellAddressAt(x, y);
        if (address == null) {
            return;
        }

        if (activeSelectionHandle == HANDLE_DYNAMIC) {
            if (comparePosition(
                    address.documentLine,
                    address.column,
                    selectionBaseStartLine,
                    selectionBaseStartColumn) < 0) {
                selectionStartLine = address.documentLine;
                selectionStartColumn = address.column;
                selectionEndLine = selectionBaseEndLine;
                selectionEndColumn = selectionBaseEndColumn;
            } else if (comparePosition(
                    address.documentLine,
                    address.trailingColumn,
                    selectionBaseEndLine,
                    selectionBaseEndColumn) > 0) {
                selectionStartLine = selectionBaseStartLine;
                selectionStartColumn = selectionBaseStartColumn;
                selectionEndLine = address.documentLine;
                selectionEndColumn = address.trailingColumn;
            } else {
                selectionStartLine = selectionBaseStartLine;
                selectionStartColumn = selectionBaseStartColumn;
                selectionEndLine = selectionBaseEndLine;
                selectionEndColumn = selectionBaseEndColumn;
            }
        } else if (activeSelectionHandle == HANDLE_START) {
            if (comparePosition(
                    address.documentLine,
                    address.column,
                    selectionEndLine,
                    selectionEndColumn) <= 0) {
                selectionStartLine = address.documentLine;
                selectionStartColumn = address.column;
            } else {
                selectionStartLine = selectionEndLine;
                selectionStartColumn = selectionEndColumn;
                selectionEndLine = address.documentLine;
                selectionEndColumn = address.trailingColumn;
                activeSelectionHandle = HANDLE_END;
            }
        } else if (activeSelectionHandle == HANDLE_END) {
            if (comparePosition(
                    address.documentLine,
                    address.trailingColumn,
                    selectionStartLine,
                    selectionStartColumn) >= 0) {
                selectionEndLine = address.documentLine;
                selectionEndColumn = address.trailingColumn;
            } else {
                selectionEndLine = selectionStartLine;
                selectionEndColumn = selectionStartColumn;
                selectionStartLine = address.documentLine;
                selectionStartColumn = address.column;
                activeSelectionHandle = HANDLE_START;
            }
        }
        invalidateSelectionActionMode();
        invalidate();
    }

    private CellAddress cellAddressAt(float x, float y) {
        if (cellWidth <= 0f || cellHeight <= 0f) {
            return null;
        }
        int offsetLines = Math.round(scrollOffsetPixels / cellHeight);
        TerminalBuffer.Snapshot snapshot = buffer.snapshot(offsetLines);
        int row = (int) Math.floor((y - getPaddingTop()) / cellHeight);
        int column = (int) Math.floor((x - getPaddingLeft()) / cellWidth);
        row = Math.max(0, Math.min(snapshot.rows - 1, row));
        column = Math.max(0, Math.min(snapshot.columns - 1, column));
        if (snapshot.isWideContinuationAt(row, column) && column > 0) {
            column--;
        }
        int trailingColumn = column;
        if (column + 1 < snapshot.columns
                && snapshot.isWideContinuationAt(row, column + 1)) {
            trailingColumn++;
        }
        return new CellAddress(snapshot.firstDocumentLine + row, column, trailingColumn);
    }

    private int selectionHandleNear(float x, float y) {
        int offsetLines = cellHeight <= 0f ? 0 : Math.round(scrollOffsetPixels / cellHeight);
        TerminalBuffer.Snapshot snapshot = buffer.snapshot(offsetLines);
        float startDistance = selectionHandleDistanceSquared(
                snapshot, selectionStartLine, selectionStartColumn, false, x, y);
        float endDistance = selectionHandleDistanceSquared(
                snapshot, selectionEndLine, selectionEndColumn, true, x, y);
        float radius = Math.max(dp(24f), cellHeight * 1.15f);
        float maximumDistance = radius * radius;
        if (startDistance > maximumDistance && endDistance > maximumDistance) {
            return HANDLE_NONE;
        }
        return startDistance <= endDistance ? HANDLE_START : HANDLE_END;
    }

    private float selectionHandleDistanceSquared(
            TerminalBuffer.Snapshot snapshot,
            int documentLine,
            int column,
            boolean trailing,
            float touchX,
            float touchY) {
        int row = documentLine - snapshot.firstDocumentLine;
        if (row < 0 || row >= snapshot.rows) {
            return Float.MAX_VALUE;
        }
        float handleX = getPaddingLeft() + (column + (trailing ? 1 : 0)) * cellWidth;
        float radius = Math.max(dp(4f), Math.min(cellWidth, cellHeight) * 0.22f);
        float handleY = getPaddingTop() + (row + 1) * cellHeight - radius;
        float dx = touchX - handleX;
        float dy = touchY - handleY;
        return dx * dx + dy * dy;
    }

    private void ensureSelectionActionMode() {
        if (selectionActionMode == null) {
            selectionActionMode = startActionMode(
                    selectionActionCallback,
                    ActionMode.TYPE_FLOATING);
        }
    }

    private void invalidateSelectionActionMode() {
        ActionMode mode = selectionActionMode;
        if (mode != null) {
            mode.invalidate();
            mode.invalidateContentRect();
        }
    }

    private void clearSelectionState() {
        selectionActive = false;
        selectionDragging = false;
        activeSelectionHandle = HANDLE_NONE;
        invalidate();
    }

    private void selectionContentRect(Rect output) {
        if (!selectionActive || cellWidth <= 0f || cellHeight <= 0f) {
            int centerX = getWidth() / 2;
            int centerY = getHeight() / 2;
            output.set(centerX, centerY, centerX + 1, centerY + 1);
            return;
        }
        int offsetLines = Math.round(scrollOffsetPixels / cellHeight);
        TerminalBuffer.Snapshot snapshot = buffer.snapshot(offsetLines);
        int firstVisible = snapshot.firstDocumentLine;
        int lastVisible = firstVisible + snapshot.rows - 1;
        int first = Math.max(selectionStartLine, firstVisible);
        int last = Math.min(selectionEndLine, lastVisible);
        if (first > last) {
            int centerX = getWidth() / 2;
            int centerY = getHeight() / 2;
            output.set(centerX, centerY, centerX + 1, centerY + 1);
            return;
        }
        int topRow = first - firstVisible;
        int bottomRow = last - firstVisible;
        int leftColumn = first == selectionStartLine ? selectionStartColumn : 0;
        int rightColumn = last == selectionEndLine
                ? selectionEndColumn + 1
                : snapshot.columns;
        if (first != last) {
            leftColumn = 0;
            rightColumn = snapshot.columns;
        }
        output.set(
                Math.round(getPaddingLeft() + leftColumn * cellWidth),
                Math.round(getPaddingTop() + topRow * cellHeight),
                Math.round(getPaddingLeft() + rightColumn * cellWidth),
                Math.round(getPaddingTop() + (bottomRow + 1) * cellHeight));
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

    private void refreshAfterOutput(final int historyBefore) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                // Output can mutate cells or evict the oldest bounded-history row. Ending a
                // selection avoids silently copying different text than the cells the user saw.
                clearSelection();
                int added = Math.max(0, buffer.getScrollbackSize() - historyBefore);
                if (scrollOffsetPixels > 0 && added > 0) {
                    scrollOffsetPixels += Math.round(added * cellHeight);
                }
                clampScrollOffset();
                restartCursorBlink();
                invalidate();
            }
        });
    }

    private void restartCursorBlink() {
        blinkPhase = true;
        removeCallbacks(cursorBlink);
        if (cursorBlinkEnabled && attached) {
            postDelayed(cursorBlink, CURSOR_BLINK_MILLIS);
        }
    }

    private void setScrollOffsetPixels(int requestedOffset) {
        int maximum = Math.round(buffer.getScrollbackSize() * cellHeight);
        scrollOffsetPixels = Math.max(0, Math.min(maximum, requestedOffset));
        invalidate();
    }

    private void clampScrollOffset() {
        setScrollOffsetPixels(scrollOffsetPixels);
    }

    private void dispatchInput(byte[] bytes) {
        if (bytes.length == 0) {
            return;
        }
        clearSelection();
        scrollToBottom();
        restartCursorBlink();
        InputListener listener = inputListener;
        if (listener != null) {
            listener.onTerminalInput(bytes);
        }
    }

    private void sendTextWithModifiers(String text, boolean control, boolean alt) {
        if (!control && !alt) {
            dispatchInput(text.getBytes(StandardCharsets.UTF_8));
            return;
        }
        for (int offset = 0; offset < text.length(); ) {
            int codePoint = text.codePointAt(offset);
            offset += Character.charCount(codePoint);
            sendCodePointWithModifiers(codePoint, control, alt);
        }
    }

    private void sendCodePointWithModifiers(int codePoint, boolean control, boolean alt) {
        byte[] encoded;
        int controlCode = control ? toControlCode(codePoint) : -1;
        if (controlCode >= 0) {
            encoded = new byte[] {(byte) controlCode};
        } else {
            encoded = new String(Character.toChars(codePoint)).getBytes(StandardCharsets.UTF_8);
        }
        if (alt) {
            byte[] prefixed = new byte[encoded.length + 1];
            prefixed[0] = 0x1b;
            System.arraycopy(encoded, 0, prefixed, 1, encoded.length);
            encoded = prefixed;
        }
        dispatchInput(encoded);
    }

    private void sendSpecialKey(
            SpecialKey key,
            boolean shift,
            boolean alt,
            boolean control) {
        String sequence;
        switch (key) {
            case ESCAPE: sequence = "\u001b"; break;
            case TAB: sequence = shift ? "\u001b[Z" : "\t"; break;
            case ENTER: sequence = "\r"; break;
            case BACKSPACE: sequence = "\u007f"; break;
            case DELETE: sequence = "\u001b[3~"; break;
            case INSERT: sequence = "\u001b[2~"; break;
            case HOME: sequence = modifiedCsi('H', shift, alt, control); break;
            case END: sequence = modifiedCsi('F', shift, alt, control); break;
            case UP: sequence = modifiedCsi('A', shift, alt, control); break;
            case DOWN: sequence = modifiedCsi('B', shift, alt, control); break;
            case RIGHT: sequence = modifiedCsi('C', shift, alt, control); break;
            case LEFT: sequence = modifiedCsi('D', shift, alt, control); break;
            case PAGE_UP: sequence = "\u001b[5~"; break;
            case PAGE_DOWN: sequence = "\u001b[6~"; break;
            case F1: sequence = "\u001bOP"; break;
            case F2: sequence = "\u001bOQ"; break;
            case F3: sequence = "\u001bOR"; break;
            case F4: sequence = "\u001bOS"; break;
            case F5: sequence = "\u001b[15~"; break;
            case F6: sequence = "\u001b[17~"; break;
            case F7: sequence = "\u001b[18~"; break;
            case F8: sequence = "\u001b[19~"; break;
            case F9: sequence = "\u001b[20~"; break;
            case F10: sequence = "\u001b[21~"; break;
            case F11: sequence = "\u001b[23~"; break;
            case F12: sequence = "\u001b[24~"; break;
            default: return;
        }
        dispatchInput(sequence.getBytes(StandardCharsets.UTF_8));
    }

    private static String modifiedCsi(
            char finalCharacter,
            boolean shift,
            boolean alt,
            boolean control) {
        int modifier = 1 + (shift ? 1 : 0) + (alt ? 2 : 0) + (control ? 4 : 0);
        if (modifier == 1) {
            return "\u001b[" + finalCharacter;
        }
        return "\u001b[1;" + modifier + finalCharacter;
    }

    private static SpecialKey specialKeyForAndroidCode(int keyCode) {
        switch (keyCode) {
            case KeyEvent.KEYCODE_ESCAPE: return SpecialKey.ESCAPE;
            case KeyEvent.KEYCODE_TAB: return SpecialKey.TAB;
            case KeyEvent.KEYCODE_ENTER:
            case KeyEvent.KEYCODE_NUMPAD_ENTER: return SpecialKey.ENTER;
            case KeyEvent.KEYCODE_DEL: return SpecialKey.BACKSPACE;
            case KeyEvent.KEYCODE_FORWARD_DEL: return SpecialKey.DELETE;
            case KeyEvent.KEYCODE_INSERT: return SpecialKey.INSERT;
            case KeyEvent.KEYCODE_DPAD_UP: return SpecialKey.UP;
            case KeyEvent.KEYCODE_DPAD_DOWN: return SpecialKey.DOWN;
            case KeyEvent.KEYCODE_DPAD_LEFT: return SpecialKey.LEFT;
            case KeyEvent.KEYCODE_DPAD_RIGHT: return SpecialKey.RIGHT;
            case KeyEvent.KEYCODE_MOVE_HOME: return SpecialKey.HOME;
            case KeyEvent.KEYCODE_MOVE_END: return SpecialKey.END;
            case KeyEvent.KEYCODE_PAGE_UP: return SpecialKey.PAGE_UP;
            case KeyEvent.KEYCODE_PAGE_DOWN: return SpecialKey.PAGE_DOWN;
            case KeyEvent.KEYCODE_F1: return SpecialKey.F1;
            case KeyEvent.KEYCODE_F2: return SpecialKey.F2;
            case KeyEvent.KEYCODE_F3: return SpecialKey.F3;
            case KeyEvent.KEYCODE_F4: return SpecialKey.F4;
            case KeyEvent.KEYCODE_F5: return SpecialKey.F5;
            case KeyEvent.KEYCODE_F6: return SpecialKey.F6;
            case KeyEvent.KEYCODE_F7: return SpecialKey.F7;
            case KeyEvent.KEYCODE_F8: return SpecialKey.F8;
            case KeyEvent.KEYCODE_F9: return SpecialKey.F9;
            case KeyEvent.KEYCODE_F10: return SpecialKey.F10;
            case KeyEvent.KEYCODE_F11: return SpecialKey.F11;
            case KeyEvent.KEYCODE_F12: return SpecialKey.F12;
            default: return null;
        }
    }

    private static int toControlCode(int codePoint) {
        int upper = Character.toUpperCase(codePoint);
        if (upper >= 'A' && upper <= 'Z') {
            return upper - 'A' + 1;
        }
        switch (codePoint) {
            case ' ':
            case '@': return 0;
            case '[': return 0x1b;
            case '\\': return 0x1c;
            case ']': return 0x1d;
            case '^': return 0x1e;
            case '_': return 0x1f;
            case '?': return 0x7f;
            default: return -1;
        }
    }

    private void applyComposingText(String next, boolean committed) {
        int common = commonPrefixAtCodePointBoundary(composingText, next);
        int oldSuffixCodePoints = composingText.codePointCount(common, composingText.length());
        if (oldSuffixCodePoints > 0) {
            byte[] deletes = new byte[Math.min(MAX_IME_DELETE, oldSuffixCodePoints)];
            Arrays.fill(deletes, (byte) 0x7f);
            dispatchInput(deletes);
        }
        if (common < next.length()) {
            sendTextWithModifiers(next.substring(common), virtualControl, virtualAlt);
        }
        composingText = committed ? "" : next;
        imeEditable.clear();
        if (!committed) {
            imeEditable.append(next);
        }
    }

    private static int commonPrefixAtCodePointBoundary(String left, String right) {
        int maximum = Math.min(left.length(), right.length());
        int index = 0;
        while (index < maximum) {
            int leftCodePoint = left.codePointAt(index);
            int rightCodePoint = right.codePointAt(index);
            if (leftCodePoint != rightCodePoint) {
                break;
            }
            index += Character.charCount(leftCodePoint);
        }
        return index;
    }

    private static boolean containsControlCharacter(String text) {
        for (int index = 0; index < text.length(); index++) {
            char character = text.charAt(index);
            if (character < 0x20 || character == 0x7f) {
                return true;
            }
        }
        return false;
    }

    private void runOnUiThread(Runnable action) {
        if (Looper.myLooper() == Looper.getMainLooper()) {
            action.run();
        } else {
            post(action);
        }
    }

    private float dp(float value) {
        return TypedValue.applyDimension(
                TypedValue.COMPLEX_UNIT_DIP,
                value,
                getResources().getDisplayMetrics());
    }

    private final class TerminalGestureListener extends GestureDetector.SimpleOnGestureListener {
        @Override
        public boolean onDown(MotionEvent event) {
            scroller.abortAnimation();
            requestFocus();
            if (getParent() != null) {
                getParent().requestDisallowInterceptTouchEvent(true);
            }
            return true;
        }

        @Override
        public boolean onSingleTapUp(MotionEvent event) {
            if (selectionActive) {
                clearSelection();
            }
            performClick();
            return true;
        }

        @Override
        public void onLongPress(MotionEvent event) {
            startWordSelection(event.getX(), event.getY());
        }

        @Override
        public boolean onScroll(
                MotionEvent first,
                MotionEvent current,
                float distanceX,
                float distanceY) {
            if (selectionDragging) {
                updateSelectionDrag(current.getX(), current.getY());
                return true;
            }
            setScrollOffsetPixels(scrollOffsetPixels + Math.round(distanceY));
            return true;
        }

        @Override
        public boolean onFling(
                MotionEvent first,
                MotionEvent current,
                float velocityX,
                float velocityY) {
            int maximum = Math.round(buffer.getScrollbackSize() * cellHeight);
            scroller.fling(
                    0,
                    scrollOffsetPixels,
                    0,
                    Math.round(-velocityY),
                    0,
                    0,
                    0,
                    maximum);
            postInvalidateOnAnimation();
            return true;
        }
    }
}
