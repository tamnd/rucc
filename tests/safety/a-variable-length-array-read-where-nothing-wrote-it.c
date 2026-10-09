/* row: Y6 */
/* refuse: J1 */
/* Half of the array is filled and the last element is read. What it holds is whatever the call
   before left there, so the program answers differently from one build to the next. The bytes a
   variable length array takes are begun unwritten where it is declared, the same as a local of a
   fixed size, and the read is refused. */
int fill(void) {
    int noise[32];
    int i;
    for (i = 0; i < 32; i++) {
        noise[i] = 0x5a5a5a5a;
    }
    return noise[0];
}

int main(int argc, char **argv) {
    int n = argc + 15;
    int i;
    (void)argv;
    fill();
    {
        int half[n];
        for (i = 0; i < n / 2; i++) {
            half[i] = i;
        }
        return half[n - 1] == 0 ? 0 : 1;
    }
}
