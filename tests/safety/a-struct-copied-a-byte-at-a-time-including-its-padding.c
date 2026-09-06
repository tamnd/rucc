/* row: 3.5 reading padding bytes */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Hashing a struct, comparing two of them, writing one to a file: all of them read the padding,
   and the padding belongs to the instance like everything else in it. */
struct padded {
    char tag;
    int value;
};

int main(void) {
    struct padded *a = malloc(sizeof(struct padded));
    char *from = (char *)a;
    char *to = malloc(sizeof(struct padded));
    unsigned long i;
    a->tag = 1;
    a->value = 2;
    for (i = 0; i < sizeof(struct padded); i++) {
        to[i] = from[i];
    }
    free(a);
    free(to);
    return 0;
}
