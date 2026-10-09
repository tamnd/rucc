/* row: T4 */
/* refuse: J1 */
/* The same escape with nothing stopping the inliner. At -O2 the callee's buffer becomes a slot of
   the caller's frame, which goes on after the copy of the callee has finished, so the witness the
   frame closes says nothing. The inliner ends the buffer's lifetime where the callee returned and
   that is where its own witness is shut. */
static int *escape(int n) {
    int local[16];
    local[0] = n;
    return local;
}

int main(int argc, char **argv) {
    int *p = escape(argc);
    (void)argv;
    return p[0];
}
