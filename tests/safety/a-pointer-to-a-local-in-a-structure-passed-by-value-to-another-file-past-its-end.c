/* row: S2 */
/* refuse: J1 */
/* with: carry */
struct holder {
    char *data;
};
void carry(struct holder held, int count);
/* The buffer's address goes into a structure and the structure goes by value to a function in
   another file, which writes one byte too many through it. The structure travels in a register
   as a pointer, so the buffer's bounds go with it in the frame. */
int main(void) {
    char buffer[10];
    struct holder held;
    held.data = buffer;
    carry(held, 11);
    return buffer[0];
}
