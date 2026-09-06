/* row: Y7 */
/* refuse: J1 */
/* gap: #431 */
void *malloc(unsigned long size);
void free(void *p);
/* An uninitialized read restricted to a pointer shaped slot, which is the one that matters most
   because the value gets followed rather than added up. malloc does not clear, and the struct
   was allocated but only half filled in. */
struct handle {
    int id;
    char *name;
};

int main(void) {
    struct handle *h = malloc(sizeof(struct handle));
    char first;
    h->id = 1;
    first = h->name[0];
    free(h);
    return first;
}
