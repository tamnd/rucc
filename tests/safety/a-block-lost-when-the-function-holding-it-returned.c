/* row: T9 */
/* flags: -fsafety-leaks */
/* leak */
/* says: row T9 */
/* says:   64 bytes at 0x */
/* says:   64 bytes in 1 allocation */
void *malloc(unsigned long size);
/* CWE-401 in its plainest form. The only pointer to the block was a local of a function that has
   returned, and main has returned too, so nothing the program can still reach points at it. The
   frame that held it is still there under the C library's, and a sweep that scanned it would call
   the block reachable, which is why the stack is not a root once main is gone. */
__attribute__((noinline)) static int fill(void) {
    char *block = malloc(64);
    for (int i = 0; i < 64; i++)
        block[i] = (char)i;
    return block[10];
}

int main(void) {
    return fill() == 10 ? 0 : 1;
}
