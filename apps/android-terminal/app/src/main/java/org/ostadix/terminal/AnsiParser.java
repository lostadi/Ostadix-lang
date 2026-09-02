package org.ostadix.terminal;

import java.nio.ByteBuffer;
import java.nio.CharBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CoderResult;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.nio.charset.CharsetDecoder;
import java.util.Arrays;

/** Stateful UTF-8 and bounded ANSI/VT stream decoder. */
public final class AnsiParser {
    private static final int MAX_DECODE_CHUNK = 8 * 1024;
    private static final int MAX_CSI_LENGTH = 256;
    private static final int MAX_CSI_PARAMETERS = 32;
    private static final int MAX_PARAMETER_VALUE = 65_535;
    private static final int MAX_OSC_LENGTH = 4 * 1024;
    private static final int MAX_TITLE_LENGTH = 256;

    private static final int STATE_TEXT = 0;
    private static final int STATE_ESCAPE = 1;
    private static final int STATE_CSI = 2;
    private static final int STATE_CSI_DISCARD = 3;
    private static final int STATE_OSC = 4;
    private static final int STATE_OSC_ESCAPE = 5;
    private static final int STATE_STRING_DISCARD = 6;
    private static final int STATE_STRING_DISCARD_ESCAPE = 7;
    private static final int STATE_CHARSET_G0 = 8;
    private static final int STATE_CHARSET_G1 = 9;
    private static final int STATE_ESCAPE_INTERMEDIATE = 10;

    public interface TitleListener {
        void onTitleChanged(String title);
    }

    public interface BellListener {
        void onBell();
    }

    /** Receives only fixed, bounded replies to standard terminal queries. */
    public interface ResponseListener {
        void onResponse(byte[] response);
    }

    private final TerminalBuffer buffer;
    private final CharsetDecoder decoder = StandardCharsets.UTF_8.newDecoder()
            .onMalformedInput(CodingErrorAction.REPLACE)
            .onUnmappableCharacter(CodingErrorAction.REPLACE);
    private final byte[] carry = new byte[8];
    private int carryLength;

    private final StringBuilder sequence = new StringBuilder();
    private int state = STATE_TEXT;
    private boolean oscOverflow;
    private char pendingHighSurrogate;
    private boolean g0DecGraphics;
    private boolean g1DecGraphics;
    private boolean useG1;
    private boolean alternateCharsetSaved;
    private boolean savedG0DecGraphics;
    private boolean savedG1DecGraphics;
    private boolean savedUseG1;

    private volatile TitleListener titleListener;
    private volatile BellListener bellListener;
    private volatile ResponseListener responseListener;

    public AnsiParser(TerminalBuffer buffer) {
        if (buffer == null) {
            throw new IllegalArgumentException("buffer must not be null");
        }
        this.buffer = buffer;
    }

    public void setTitleListener(TitleListener listener) {
        titleListener = listener;
    }

    public void setBellListener(BellListener listener) {
        bellListener = listener;
    }

    public void setResponseListener(ResponseListener listener) {
        responseListener = listener;
    }

    /** Feeds a possibly partial UTF-8 chunk. Incomplete code units are retained for the next call. */
    public synchronized void feed(byte[] bytes) {
        if (bytes == null) {
            throw new IllegalArgumentException("bytes must not be null");
        }
        feed(bytes, 0, bytes.length);
    }

    public synchronized void feed(byte[] bytes, int offset, int length) {
        if (bytes == null) {
            throw new IllegalArgumentException("bytes must not be null");
        }
        if (offset < 0 || length < 0 || offset > bytes.length - length) {
            throw new IndexOutOfBoundsException("invalid byte range");
        }
        int position = offset;
        int remaining = length;
        while (remaining > 0) {
            int count = Math.min(remaining, MAX_DECODE_CHUNK);
            decodeChunk(bytes, position, count, false);
            position += count;
            remaining -= count;
        }
    }

    /** Flushes a completed PTY stream, replacing any truncated UTF-8 tail. */
    public synchronized void finish() {
        decodeChunk(new byte[0], 0, 0, true);
        if (pendingHighSurrogate != 0) {
            acceptCodePoint(0xfffd);
            pendingHighSurrogate = 0;
        }
        decoder.reset();
        carryLength = 0;
    }

    /** Resets decoder and terminal state for a fresh session. */
    public synchronized void reset() {
        decoder.reset();
        carryLength = 0;
        pendingHighSurrogate = 0;
        state = STATE_TEXT;
        sequence.setLength(0);
        oscOverflow = false;
        g0DecGraphics = false;
        g1DecGraphics = false;
        useG1 = false;
        alternateCharsetSaved = false;
        savedG0DecGraphics = false;
        savedG1DecGraphics = false;
        savedUseG1 = false;
        buffer.reset();
    }

    private void decodeChunk(byte[] bytes, int offset, int length, boolean endOfInput) {
        byte[] inputBytes = new byte[carryLength + length];
        if (carryLength > 0) {
            System.arraycopy(carry, 0, inputBytes, 0, carryLength);
        }
        if (length > 0) {
            System.arraycopy(bytes, offset, inputBytes, carryLength, length);
        }

        ByteBuffer input = ByteBuffer.wrap(inputBytes);
        CharBuffer output = CharBuffer.allocate(Math.max(32, inputBytes.length + 2));
        while (true) {
            CoderResult result = decoder.decode(input, output, endOfInput);
            output.flip();
            while (output.hasRemaining()) {
                acceptChar(output.get());
            }
            output.clear();
            if (result.isOverflow()) {
                continue;
            }
            if (result.isError()) {
                try {
                    result.throwException();
                } catch (CharacterCodingException impossibleWithReplacement) {
                    acceptCodePoint(0xfffd);
                }
                if (input.hasRemaining()) {
                    input.get();
                }
                continue;
            }
            break;
        }

        if (endOfInput) {
            while (true) {
                CoderResult result = decoder.flush(output);
                output.flip();
                while (output.hasRemaining()) {
                    acceptChar(output.get());
                }
                output.clear();
                if (!result.isOverflow()) {
                    break;
                }
            }
            carryLength = 0;
        } else {
            carryLength = Math.min(input.remaining(), carry.length);
            if (carryLength > 0) {
                input.get(carry, 0, carryLength);
            }
        }
        Arrays.fill(inputBytes, (byte) 0);
    }

    private void acceptChar(char value) {
        if (pendingHighSurrogate != 0) {
            if (Character.isLowSurrogate(value)) {
                int codePoint = Character.toCodePoint(pendingHighSurrogate, value);
                pendingHighSurrogate = 0;
                acceptCodePoint(codePoint);
                return;
            }
            acceptCodePoint(0xfffd);
            pendingHighSurrogate = 0;
        }
        if (Character.isHighSurrogate(value)) {
            pendingHighSurrogate = value;
        } else if (Character.isLowSurrogate(value)) {
            acceptCodePoint(0xfffd);
        } else {
            acceptCodePoint(value);
        }
    }

    private void acceptCodePoint(int codePoint) {
        switch (state) {
            case STATE_TEXT:
                acceptText(codePoint);
                return;
            case STATE_ESCAPE:
                acceptEscape(codePoint);
                return;
            case STATE_CSI:
                acceptCsi(codePoint);
                return;
            case STATE_CSI_DISCARD:
                if (codePoint == 0x1b) {
                    state = STATE_ESCAPE;
                } else if (isCsiFinal(codePoint) || codePoint == 0x18 || codePoint == 0x1a) {
                    state = STATE_TEXT;
                }
                return;
            case STATE_OSC:
                acceptOsc(codePoint);
                return;
            case STATE_OSC_ESCAPE:
                if (codePoint == '\\' || codePoint == 0x9c) {
                    finishOsc();
                } else {
                    appendOsc(0x1b);
                    appendOsc(codePoint);
                    state = STATE_OSC;
                }
                return;
            case STATE_STRING_DISCARD:
                if (codePoint == 0x1b) {
                    state = STATE_STRING_DISCARD_ESCAPE;
                } else if (codePoint == 0x9c || codePoint == 0x18 || codePoint == 0x1a) {
                    state = STATE_TEXT;
                }
                return;
            case STATE_STRING_DISCARD_ESCAPE:
                state = codePoint == '\\' ? STATE_TEXT : STATE_STRING_DISCARD;
                return;
            case STATE_CHARSET_G0:
                g0DecGraphics = codePoint == '0';
                state = STATE_TEXT;
                return;
            case STATE_CHARSET_G1:
                g1DecGraphics = codePoint == '0';
                state = STATE_TEXT;
                return;
            case STATE_ESCAPE_INTERMEDIATE:
                state = STATE_TEXT;
                return;
            default:
                state = STATE_TEXT;
        }
    }

    private void acceptText(int codePoint) {
        switch (codePoint) {
            case 0x00:
            case 0x7f:
                return;
            case 0x07:
                BellListener bell = bellListener;
                if (bell != null) {
                    bell.onBell();
                }
                return;
            case 0x08:
                buffer.backspace();
                return;
            case 0x09:
                buffer.tab();
                return;
            case 0x0a:
            case 0x0b:
            case 0x0c:
                buffer.lineFeed();
                return;
            case 0x0d:
                buffer.carriageReturn();
                return;
            case 0x0e:
                useG1 = true;
                return;
            case 0x0f:
                useG1 = false;
                return;
            case 0x1b:
                state = STATE_ESCAPE;
                return;
            case 0x90:
            case 0x98:
            case 0x9e:
            case 0x9f:
                state = STATE_STRING_DISCARD;
                return;
            case 0x9b:
                sequence.setLength(0);
                state = STATE_CSI;
                return;
            case 0x9d:
                sequence.setLength(0);
                oscOverflow = false;
                state = STATE_OSC;
                return;
            default:
                if (codePoint >= 0x20 && codePoint != 0x9c) {
                    buffer.putCodePoint(mapDecGraphics(codePoint));
                }
        }
    }

    private void acceptEscape(int codePoint) {
        switch (codePoint) {
            case '[':
                sequence.setLength(0);
                state = STATE_CSI;
                return;
            case ']':
                sequence.setLength(0);
                oscOverflow = false;
                state = STATE_OSC;
                return;
            case 'P':
            case 'X':
            case '^':
            case '_':
                state = STATE_STRING_DISCARD;
                return;
            case '(':
                state = STATE_CHARSET_G0;
                return;
            case ')':
                state = STATE_CHARSET_G1;
                return;
            case '*':
            case '+':
            case '#':
            case '%':
                state = STATE_ESCAPE_INTERMEDIATE;
                return;
            case '7':
                buffer.saveCursor();
                break;
            case '8':
                buffer.restoreCursor();
                break;
            case 'D':
                buffer.lineFeed();
                break;
            case 'E':
                buffer.nextLine();
                break;
            case 'M':
                buffer.reverseIndex();
                break;
            case 'c':
                g0DecGraphics = false;
                g1DecGraphics = false;
                useG1 = false;
                alternateCharsetSaved = false;
                savedG0DecGraphics = false;
                savedG1DecGraphics = false;
                savedUseG1 = false;
                buffer.reset();
                break;
            case '=':
            case '>':
                break;
            case 0x1b:
                return;
            default:
                break;
        }
        state = STATE_TEXT;
    }

    private void acceptCsi(int codePoint) {
        if (codePoint == 0x18 || codePoint == 0x1a) {
            sequence.setLength(0);
            state = STATE_TEXT;
            return;
        }
        if (codePoint == 0x1b) {
            sequence.setLength(0);
            state = STATE_ESCAPE;
            return;
        }
        if (isCsiFinal(codePoint)) {
            dispatchCsi((char) codePoint, sequence.toString());
            sequence.setLength(0);
            state = STATE_TEXT;
            return;
        }
        if (codePoint < 0x20 || codePoint > 0x3f) {
            sequence.setLength(0);
            state = STATE_CSI_DISCARD;
            return;
        }
        if (sequence.length() >= MAX_CSI_LENGTH) {
            sequence.setLength(0);
            state = STATE_CSI_DISCARD;
            return;
        }
        sequence.append((char) codePoint);
    }

    private void dispatchCsi(char command, String body) {
        boolean privateMode = body.startsWith("?");
        int[] parameters = parseParameters(body);
        int first = parameter(parameters, 0, 0);
        int amount = Math.max(1, first);
        switch (command) {
            case 'A':
                buffer.moveCursor(-amount, 0);
                break;
            case 'B':
            case 'e':
                buffer.moveCursor(amount, 0);
                break;
            case 'C':
            case 'a':
                buffer.moveCursor(0, amount);
                break;
            case 'D':
                buffer.moveCursor(0, -amount);
                break;
            case 'E':
                buffer.moveCursor(amount, 0);
                buffer.setCursorColumn(1);
                break;
            case 'F':
                buffer.moveCursor(-amount, 0);
                buffer.setCursorColumn(1);
                break;
            case 'G':
            case '`':
                buffer.setCursorColumn(parameter(parameters, 0, 1));
                break;
            case 'H':
            case 'f':
                buffer.setCursor(parameter(parameters, 0, 1), parameter(parameters, 1, 1));
                break;
            case 'd':
                buffer.setCursorRow(parameter(parameters, 0, 1));
                break;
            case 'J':
                buffer.eraseDisplay(first);
                break;
            case 'K':
                buffer.eraseLine(first);
                break;
            case 'X':
                buffer.eraseCharacters(amount);
                break;
            case '@':
                buffer.insertCharacters(amount);
                break;
            case 'P':
                buffer.deleteCharacters(amount);
                break;
            case 'L':
                buffer.insertLines(amount);
                break;
            case 'M':
                buffer.deleteLines(amount);
                break;
            case 'S':
                buffer.scrollUp(amount);
                break;
            case 'T':
                buffer.scrollDown(amount);
                break;
            case 'm':
                buffer.applySgr(parameters);
                break;
            case 's':
                buffer.saveCursor();
                break;
            case 'u':
                buffer.restoreCursor();
                break;
            case 'r':
                buffer.setScrollRegion(
                        parameter(parameters, 0, 1),
                        parameter(parameters, 1, buffer.getRows()));
                break;
            case 'h':
            case 'l':
                applyMode(privateMode, parameters, command == 'h');
                break;
            case 'c':
                replyDeviceAttributes(body, first);
                break;
            case 'n':
                replyDeviceStatus(privateMode, first);
                break;
            case 'q':
                applyCursorStyle(first);
                break;
            default:
                // Unsupported modes and queries are deliberately ignored.
                break;
        }
    }

    private void applyMode(boolean privateMode, int[] parameters, boolean enabled) {
        if (!privateMode) {
            return;
        }
        for (int parameter : parameters) {
            if (parameter == 7) {
                buffer.setAutoWrap(enabled);
            } else if (parameter == 25) {
                buffer.setCursorVisible(enabled);
            } else if (parameter == 47 || parameter == 1047 || parameter == 1049) {
                if (enabled) {
                    if (!buffer.isAlternateScreen()) {
                        savedG0DecGraphics = g0DecGraphics;
                        savedG1DecGraphics = g1DecGraphics;
                        savedUseG1 = useG1;
                        alternateCharsetSaved = true;
                    }
                    buffer.enterAlternateScreen();
                } else {
                    boolean wasAlternate = buffer.isAlternateScreen();
                    buffer.exitAlternateScreen();
                    if (wasAlternate && alternateCharsetSaved) {
                        g0DecGraphics = savedG0DecGraphics;
                        g1DecGraphics = savedG1DecGraphics;
                        useG1 = savedUseG1;
                        alternateCharsetSaved = false;
                    }
                }
            } else if (parameter == 1048) {
                if (enabled) {
                    buffer.saveCursor();
                } else {
                    buffer.restoreCursor();
                }
            }
        }
    }

    private void replyDeviceAttributes(String body, int first) {
        if (body.startsWith(">")) {
            respond("\u001b[>0;4;0c");
        } else if (first == 0) {
            respond("\u001b[?1;2c");
        }
    }

    private void replyDeviceStatus(boolean privateMode, int parameter) {
        if (parameter == 5 && !privateMode) {
            respond("\u001b[0n");
        } else if (parameter == 6) {
            String prefix = privateMode ? "\u001b[?" : "\u001b[";
            respond(prefix + (buffer.getCursorRow() + 1) + ";"
                    + (buffer.getCursorColumn() + 1) + "R");
        }
    }

    private void respond(String response) {
        ResponseListener listener = responseListener;
        if (listener != null) {
            listener.onResponse(response.getBytes(StandardCharsets.UTF_8));
        }
    }

    private void applyCursorStyle(int parameter) {
        if (parameter == 3 || parameter == 4) {
            buffer.setCursorStyle(TerminalBuffer.CursorStyle.UNDERLINE);
        } else if (parameter == 5 || parameter == 6) {
            buffer.setCursorStyle(TerminalBuffer.CursorStyle.BAR);
        } else {
            buffer.setCursorStyle(TerminalBuffer.CursorStyle.BLOCK);
        }
    }

    private int[] parseParameters(String body) {
        int start = 0;
        while (start < body.length()) {
            char value = body.charAt(start);
            if (value == '?' || value == '>' || value == '!' || value == '<') {
                start++;
            } else {
                break;
            }
        }
        int end = body.length();
        while (end > start && body.charAt(end - 1) >= 0x20 && body.charAt(end - 1) <= 0x2f) {
            end--;
        }
        if (start >= end) {
            return new int[0];
        }

        int[] result = new int[Math.min(MAX_CSI_PARAMETERS, end - start + 1)];
        int count = 0;
        int value = -1;
        for (int index = start; index < end && count < MAX_CSI_PARAMETERS; index++) {
            char character = body.charAt(index);
            if (character >= '0' && character <= '9') {
                if (value < 0) {
                    value = 0;
                }
                value = Math.min(MAX_PARAMETER_VALUE, value * 10 + (character - '0'));
            } else if (character == ';' || character == ':') {
                result[count++] = value;
                value = -1;
            } else {
                // An invalid parameter byte invalidates the remainder, but never grows storage.
                break;
            }
        }
        if (count < MAX_CSI_PARAMETERS) {
            result[count++] = value;
        }
        return Arrays.copyOf(result, count);
    }

    private void acceptOsc(int codePoint) {
        if (codePoint == 0x07 || codePoint == 0x9c) {
            finishOsc();
        } else if (codePoint == 0x1b) {
            state = STATE_OSC_ESCAPE;
        } else if (codePoint == 0x18 || codePoint == 0x1a) {
            sequence.setLength(0);
            oscOverflow = false;
            state = STATE_TEXT;
        } else {
            appendOsc(codePoint);
        }
    }

    private void appendOsc(int codePoint) {
        if (oscOverflow) {
            return;
        }
        int characterCount = Character.charCount(codePoint);
        if (sequence.length() + characterCount > MAX_OSC_LENGTH) {
            sequence.setLength(0);
            oscOverflow = true;
            return;
        }
        sequence.appendCodePoint(codePoint);
    }

    private void finishOsc() {
        if (!oscOverflow) {
            String value = sequence.toString();
            int separator = value.indexOf(';');
            if (separator > 0) {
                String command = value.substring(0, separator);
                if ("0".equals(command) || "2".equals(command)) {
                    String title = sanitizeTitle(value.substring(separator + 1));
                    TitleListener listener = titleListener;
                    if (listener != null) {
                        listener.onTitleChanged(title);
                    }
                }
            }
        }
        sequence.setLength(0);
        oscOverflow = false;
        state = STATE_TEXT;
    }

    private int mapDecGraphics(int codePoint) {
        if (!(useG1 ? g1DecGraphics : g0DecGraphics) || codePoint < '`' || codePoint > '~') {
            return codePoint;
        }
        switch (codePoint) {
            case '`': return 0x25c6;
            case 'a': return 0x2592;
            case 'f': return 0x00b0;
            case 'g': return 0x00b1;
            case 'j': return 0x2518;
            case 'k': return 0x2510;
            case 'l': return 0x250c;
            case 'm': return 0x2514;
            case 'n': return 0x253c;
            case 'o': return 0x23ba;
            case 'p': return 0x23bb;
            case 'q': return 0x2500;
            case 'r': return 0x23bc;
            case 's': return 0x23bd;
            case 't': return 0x251c;
            case 'u': return 0x2524;
            case 'v': return 0x2534;
            case 'w': return 0x252c;
            case 'x': return 0x2502;
            case 'y': return 0x2264;
            case 'z': return 0x2265;
            case '{': return 0x03c0;
            case '|': return 0x2260;
            case '}': return 0x00a3;
            case '~': return 0x00b7;
            default: return codePoint;
        }
    }

    private static boolean isCsiFinal(int codePoint) {
        return codePoint >= 0x40 && codePoint <= 0x7e;
    }

    private static int parameter(int[] parameters, int index, int defaultValue) {
        if (index >= parameters.length || parameters[index] < 0) {
            return defaultValue;
        }
        return parameters[index];
    }

    private static String sanitizeTitle(String title) {
        StringBuilder safe = new StringBuilder(Math.min(title.length(), MAX_TITLE_LENGTH));
        for (int offset = 0; offset < title.length() && safe.length() < MAX_TITLE_LENGTH; ) {
            int codePoint = title.codePointAt(offset);
            offset += Character.charCount(codePoint);
            if (codePoint >= 0x20 && codePoint != 0x7f && codePoint != 0x9b && codePoint != 0x9d) {
                safe.appendCodePoint(codePoint);
            }
        }
        return safe.toString();
    }
}
