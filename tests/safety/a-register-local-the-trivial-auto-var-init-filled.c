/* row: Y6 */
/* flags: -ftrivial-auto-var-init=zero */
/* allow */
/* Under -ftrivial-auto-var-init=zero a local with no initializer is given one where it is
   declared, so a read of it before the program writes it reads that zero and is not a read of
   nothing. */
int main(int argc, char **argv) {
    int data;
    (void)argv;
    if (argc > 5) {
        data = 3;
    }
    return data;
}
