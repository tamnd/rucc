/* row: Y5 */
/* refuse: J1 */
/* gap: #431 */
void *malloc(unsigned long size);
/* A heap pointer called. Hardware page permissions stop this on any current machine, which is
   why it is easy to think the model does not need a rule for it, and why the rule is about the
   provenance class rather than about whether the page happens to be executable. */
int main(void) {
    void *data = malloc(64);
    int (*as_function)(void) = (int (*)(void))data;
    return as_function();
}
