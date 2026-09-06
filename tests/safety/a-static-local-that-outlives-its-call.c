/* row: S3 */
/* allow */
/* It is declared inside a function and it is static storage, so it lives for the whole run. A
   checker that ends its lifetime at the closing brace, the way it does for the locals beside it,
   reports a use after return on every counter and cache in C. */
int next(void) {
    static int counter = 0;
    counter++;
    return counter;
}

int main(void) {
    int i;
    for (i = 0; i < 8; i++) {
        next();
    }
    return next() == 9 ? 0 : 1;
}
