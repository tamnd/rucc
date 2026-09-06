/* The uninstrumented half of document 10 section 10.7's mixed link.
 *
 * Built by the system compiler and not by rucc, into a shared object, which is the whole point of
 * it. Nothing in here is instrumented, nothing in here knows the monitor exists, and a program that
 * links against it is the configuration section 10.7 says matters most in practice: an instrumented
 * application against uninstrumented shared libraries.
 *
 * It is deliberately the shape that is hardest for a boundary to handle. It takes a pointer the
 * program owns and writes through it, which is a transfer out. It hands a pointer back, which is
 * an address the planes know nothing about until somebody looks. And it calls back into the
 * program, which is section 10.8's case: the caller is code this compiler did not build, so there
 * is no call frame beside the call and the callee's parameters arrive with no capability at all.
 */

/* How wide one record is. Both sides agree on it by writing it down, the way two objects in a real
   link agree on the layout of anything they pass between them. */
#define NOTES_RECORD 8

/* Fills a buffer the caller owns, which is a pointer handed out of instrumented code. */
void notes_fill(char *text, unsigned long len) {
    unsigned long at = 0;
    while (at < len) {
        text[at] = (char)('a' + (at % 26));
        at += 1;
    }
}

/* Hands each fixed width record to the visitor, which is a pointer handed back in. */
void notes_each(char *text, unsigned long len, void (*visit)(char *, unsigned long)) {
    unsigned long at = 0;
    while (at + NOTES_RECORD <= len) {
        visit(text + at, NOTES_RECORD);
        at += NOTES_RECORD;
    }
}

/* How many records fit, so that a caller has a number that came from here rather than from itself. */
unsigned long notes_count(unsigned long len) { return len / NOTES_RECORD; }
