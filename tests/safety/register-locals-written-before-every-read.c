/* row: Y6 */
/* allow */
/* Locals kept in registers and written before every read, in the shapes where that has to be
   worked out rather than seen: both arms of an if, a switch with a default, a loop counter, a
   variable declared in a loop and written before it is read on the same trip, one written and
   read under the same condition, and one only cast to void. */
int main(int argc, char **argv) {
    int both;
    int chosen;
    int counter;
    int late;
    int unused;
    int sum = 0;
    (void)argv;
    (void)unused;
    if (argc > 5) {
        both = 1;
    } else {
        both = 2;
    }
    switch (argc) {
    case 1:
        chosen = 10;
        break;
    case 2:
        chosen = 20;
        break;
    default:
        chosen = 30;
        break;
    }
    for (counter = 0; counter < 4; counter++) {
        int step;
        step = counter * both;
        sum += step;
    }
    if (argc > 0) {
        late = 1;
    }
    if (argc > 0) {
        sum += late;
    }
    return sum + chosen == 23 ? 0 : 1;
}
