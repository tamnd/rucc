/* row: S2 */
/* allow */
/* with: copy */
void copy(char **where, int count);
/* The case above with the count the buffer has room for. */
int main(void) {
    char buffer[10];
    char *data = buffer;
    copy(&data, 10);
    return buffer[9] != 'x';
}
