/* row: 10.7 the mixed link */
/* refuse: J1 */
/* links: notes */
/* says: capabilities recovered at the boundary */
/* Milestone S2's exit criterion: a program built at Tier D links against an uninstrumented shared
   library, runs, and reports a genuine bug in its own code.

   The bug is entirely this program's. The library hands the visitor a record and says how many
   bytes are in it, which is true, and the visitor writes a terminator one byte past the end of what
   it was told. On the last record that byte is past the end of the whole allocation, so it belongs
   to nobody and the access is refused.

   What makes it interesting is where the pointer came from. Nothing published a call frame, because
   the code doing the calling is a shared object the system compiler built, so the parameter arrived
   with no capability and everything the check knows about it was reconstructed from the planes. The
   line printed before the walk starts is what says that happened rather than that the counter
   happened to be zero and the report came from somewhere else. */
void *malloc(unsigned long size);
void free(void *p);
long write(int fd, const void *buf, unsigned long n);
unsigned long __rucc_safety_recovered(void);

void notes_fill(char *text, unsigned long len);
void notes_each(char *text, unsigned long len, void (*visit)(char *, unsigned long));

/* Called by the library. The off by one is here, in code this compiler built and checked. */
static void terminate(char *record, unsigned long len) { record[len] = 0; }

int main(void) {
    char *buffer = malloc(64);
    if (buffer == 0) {
        return 1;
    }
    notes_fill(buffer, 64);
    if (__rucc_safety_recovered() == 0) {
        write(2, "nothing crossed the boundary\n", 29);
        return 1;
    }
    write(2, "capabilities recovered at the boundary\n", 39);
    notes_each(buffer, 64, terminate);
    free(buffer);
    return 0;
}
