/* row: 10.7 the mixed link */
/* allow */
/* links: notes */
/* summary: "notes_fill" */
/* summary: "crossings": { "entered": 1, "returned": 1 } */
/* Document 10 section 10.7's incremental adoption, demonstrated rather than asserted. This program
   is built at Tier D and linked against a shared object the system compiler built, which knows
   nothing about the monitor and was not rebuilt for it. The link succeeds because section 5.3's
   calling convention is unchanged, and the program runs and is checked throughout its own code.

   It exercises all three directions in one go. It hands the library a pointer it owns, it takes a
   pointer back from the library, and it is called back with a pointer by code that published no
   call frame because it did not know to. Nothing here is a bug, so nothing is reported, and the
   exit status is what says the crossings were counted: a run where the counter never moved means
   the boundary was not being watched and the silence proved nothing.

   The two summary lines hold the build's own account of itself to the same story. The library is
   named among the things this build trusts without checking, and both directions of the crossing
   are counted, so the report says what the guarantee rests on rather than only that there is one. */
void *malloc(unsigned long size);
void free(void *p);
long write(int fd, const void *buf, unsigned long n);
unsigned long __rucc_safety_recovered(void);

void notes_fill(char *text, unsigned long len);
void notes_each(char *text, unsigned long len, void (*visit)(char *, unsigned long));
unsigned long notes_count(unsigned long len);

static long total;

/* Called by the library, so its parameters arrive with no capability beside them. */
static void add(char *record, unsigned long len) {
    unsigned long at;
    for (at = 0; at < len; at++) {
        total += record[at];
    }
}

int main(void) {
    char *buffer = malloc(64);
    if (buffer == 0) {
        return 1;
    }
    notes_fill(buffer, 64);
    notes_each(buffer, 64, add);
    if (notes_count(64) != 8) {
        return 1;
    }
    if (total == 0) {
        return 1;
    }
    if (__rucc_safety_recovered() == 0) {
        write(2, "nothing crossed the boundary\n", 29);
        return 1;
    }
    write(2, "capabilities recovered at the boundary\n", 39);
    free(buffer);
    return 0;
}
