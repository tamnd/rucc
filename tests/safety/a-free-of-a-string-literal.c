/* row: T3 */
/* refuse: J6 */
void free(void *p);
/* The default that was never strdup'd. The pointer is read only memory in a section the loader
   mapped, and the allocator has no idea what to do with it. */
int main(void) {
    const char *name = "default";
    free((void *)name);
    return 0;
}
