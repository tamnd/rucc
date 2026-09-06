/* row: 3.5 flexible array members */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* The declared extent understates the real one on purpose, and the bounds come from the
   allocation rather than from the type, so the tail is where it says it is. */
struct message {
    int length;
    char body[];
};

int main(void) {
    struct message *m = malloc(sizeof(struct message) + 32);
    int i;
    m->length = 32;
    for (i = 0; i < 32; i++) {
        m->body[i] = (char)i;
    }
    free(m);
    return 0;
}
