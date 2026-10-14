/* row: S2 */
/* allow */
/* with: carry */
struct holder {
    char *data;
};
void carry(struct holder held, int count);
/* The case above with the count the buffer has room for. */
int main(void) {
    char buffer[10];
    struct holder held;
    held.data = buffer;
    carry(held, 10);
    return buffer[9] != 'x';
}
