/*
 * composer-provider.c — a native provider fixture for test-agent-identity.py.
 *
 * It stands in for Claude Code or Codex in a real tmux pane. It records every
 * byte it receives, and it draws a minimal composer the way the real providers
 * draw theirs (recorded from live panes on 2026-09-26, SH-780), so that
 * storyhook's composer reader sees what a delivery did:
 *
 *   claude   U+276F then U+00A0 NBSP; the submit key is CR
 *   codex    U+203A then a space;     the submit key is Tab
 *
 * The provider is chosen by the executable's own name: the test copies this one
 * binary as both, because agent identity checks the executable a pane runs.
 * Typed or pasted text (bracketed paste, which it turns on) is echoed on the
 * composer row, a pasted line break as a space; the submit key outside a paste
 * clears the row. Other escape sequences are recorded but not drawn.
 *
 * usage: <codex|claude> <record-file> [<screen-file>]
 *
 * A screen file is drawn in place of the idle composer row: a dialog, or a
 * ghost suggestion. Everything is drawn BEFORE "READY" is recorded, so a test
 * that waits for READY also waits for the screen. It sends no terminal queries:
 * their replies would arrive as input and be recorded.
 */
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <termios.h>
#include <unistd.h>

static char draft[65536];
static size_t draft_length;

static int out(const char *bytes, size_t n) {
    return write(1, bytes, n) == (ssize_t)n ? 0 : -1;
}

static int outs(const char *text) { return out(text, strlen(text)); }

/* Back to column 0, erase the row, and draw the composer again. Raw mode: no
 * newline translation, so every movement is explicit. */
static int redraw(const char *glyph, const char *pad) {
    if (outs("\r\033[2K") || outs(glyph) || outs(pad)) return -1;
    return out(draft, draft_length);
}

static int draw_screen(const char *path) {
    char bytes[4096];
    ssize_t n;
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    while ((n = read(fd, bytes, sizeof(bytes))) > 0)
        if (out(bytes, (size_t)n)) return -1;
    close(fd);
    return n < 0 ? -1 : 0;
}

int main(int argc, char **argv) {
    static const char paste_begin[] = "\033[200~", paste_end[] = "\033[201~";
    if (argc != 2 && argc != 3) return 2;
    const char *name = strrchr(argv[0], '/');
    name = name ? name + 1 : argv[0];
    int claude = strcmp(name, "claude") == 0;
    const char *glyph = claude ? "\xe2\x9d\xaf" : "\xe2\x80\xba";
    const char *pad = claude ? "\xc2\xa0" : " ";
    const char submit = claude ? '\r' : '\t';

    struct termios tty;
    if (tcgetattr(0, &tty) != 0) return 3;
    cfmakeraw(&tty);
    if (tcsetattr(0, TCSANOW, &tty) != 0) return 4;
    if (outs("\033[?2004h")) return 8;
    if (argc == 3 ? draw_screen(argv[2]) : (outs("\r\n") || redraw(glyph, pad))) return 9;
    int fd = open(argv[1], O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (fd < 0) return 5;
    if (write(fd, "READY\n", 6) != 6) return 6;

    char bytes[4096], held[sizeof(paste_begin)];
    size_t held_length = 0;
    int pasting = 0;
    ssize_t n;
    while ((n = read(0, bytes, sizeof(bytes))) > 0) {
        if (write(fd, bytes, (size_t)n) != n) return 7;
        for (ssize_t i = 0; i < n; i++) {
            char c = bytes[i];
            if (held_length > 0 || c == '\033') {
                /* Only the marker that can come next is recognised; any other
                 * escape sequence is dropped from the drawing. */
                const char *marker = pasting ? paste_end : paste_begin;
                held[held_length++] = c;
                if (memcmp(held, marker, held_length) != 0) {
                    held_length = 0;
                } else if (held_length == sizeof(paste_begin) - 1) {
                    pasting = !pasting;
                    held_length = 0;
                }
                continue;
            }
            if (!pasting && c == submit)
                draft_length = 0;
            else if (draft_length < sizeof(draft))
                draft[draft_length++] = (c == '\r' || c == '\n') ? ' ' : c;
        }
        if (redraw(glyph, pad)) return 10;
    }
    return 0;
}
