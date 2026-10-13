/* row: S2 */
/* allow */
/* with: follow */
void follow(char **where, int count);
/* The case above with the count the buffer has room for, so the capability found in the table has
   to be the buffer's own and not something narrower. */
int main(void) {
    char buffer[10];
    char *data = buffer;
    follow(&data, 10);
    return buffer[9] != 'x';
}
