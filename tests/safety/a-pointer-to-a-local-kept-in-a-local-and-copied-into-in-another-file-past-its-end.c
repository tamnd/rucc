/* row: S2 */
/* refuse: J1 */
/* with: copy */
void copy(char **where, int count);
/* The buffer's address goes into a pointer variable and the variable's address goes to a function
   in another file, which reads the pointer back and hands it straight to `memcpy` with one byte too
   many. Nothing checks the pointer there, so its capability has to be handed to the wrapper from
   the `cap_load` beside the read. */
int main(void) {
    char buffer[10];
    char *data = buffer;
    copy(&data, 11);
    return buffer[0];
}
