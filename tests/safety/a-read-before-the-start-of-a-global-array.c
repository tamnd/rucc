/* row: S3 */
/* refuse: J1 */
/* Underflow of a static object reads whatever the linker put before it, which on a real program
   is another global and not anything the fault handler will notice. The capability of the variable
   is what knows where it starts. */
int before[4];
int table[16];

int main(void) {
    before[0] = 9;
    return table[-4];
}
