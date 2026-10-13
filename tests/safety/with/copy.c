/* The other half of the cases that hand a pointer kept in a word straight to `memcpy`. It reads the
   pointer out of the word it was given and copies `count` bytes into it, with no check of its own
   on the pointer, so the wrapper is the only thing that judges the copy. */
#include <string.h>
void copy(char **where, int count) {
    char source[16] = "xxxxxxxxxxxxxxx";
    char *to = *where;
    memcpy(to, source, count);
}
