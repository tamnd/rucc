/* row: Y6 */
/* refuse: J1 */
/* Each trip round the loop has a new object, so what the first trip wrote is not the second
   trip's. The second trip reads a variable nothing wrote, even though the same name was written
   the time before. */
int main(int argc, char **argv) {
    int total = 0;
    int i;
    (void)argv;
    for (i = 0; i < 3; i++) {
        int last;
        if (i == 0) {
            last = argc;
        }
        total += last;
    }
    return total == 3 ? 0 : 1;
}
