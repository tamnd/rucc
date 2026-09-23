/* row: Y2 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *p);
/* A callee that frees nothing still writes. The caller reads the bytes as `int`, the callee stores
   a `float` over them, and the caller reads them as `int` again. The first read passes and says
   the bytes are `int`, and a call that could not have freed anything was taken to leave that
   standing. It is noinline so the store stays on the other side of a call. */
__attribute__((noinline)) void store_a_float(void *raw) {
    *(float *)raw = 1.0f;
}

int main(void) {
    int *p = malloc(sizeof(int));
    int first;
    *p = 1;
    first = *p;
    store_a_float(p);
    first += *p;
    free(p);
    return first;
}
