/* row: T1 */
/* refuse: J1 */
/* says: which has been freed */
void *malloc(unsigned long size);
void free(void *p);
/* A refcount decremented after the object went away. The write lands on a member rather than on
   the base, so the address in the report is not the one that was passed to free. */
struct object {
    int refcount;
    int payload[8];
};

int main(void) {
    struct object *o = malloc(sizeof(struct object));
    o->refcount = 1;
    free(o);
    o->refcount = 0;
    return 0;
}
