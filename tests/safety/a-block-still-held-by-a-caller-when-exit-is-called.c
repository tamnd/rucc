/* row: T9 */
/* flags: -fsafety-leaks */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
void exit(int status);
/* The program leaves through exit from two calls down, and main's frame is still live and still
   holds the only pointer to the block. Valgrind calls that still reachable and so does this: the
   call to exit is marked, and the stack from there up is a root. */
__attribute__((noinline)) static void leave(char **kept) {
    if (kept[0][0] == 1)
        exit(0);
}

__attribute__((noinline)) static void deeper(char **kept) {
    leave(kept);
}

int main(void) {
    char *kept[1];
    kept[0] = malloc(32);
    kept[0][0] = 1;
    deeper(kept);
    free(kept[0]);
    return 1;
}
