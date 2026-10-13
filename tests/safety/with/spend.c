/* The other half of the case that keeps a pointer to a local in a global. It writes `count` bytes
   through whatever the global holds. */
char *kept;
void spend(int count) {
    for (int i = 0; i < count; i++)
        kept[i] = 'x';
}
