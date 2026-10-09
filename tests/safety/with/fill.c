void *memcpy(void *to, const void *from, unsigned long count);
/* The other half of the cases that hand a local to a function in another file. It copies however
   many bytes it is told to, and knows nothing about where `to` came from but what the frame says. */
void fill(char *to, const char *from, unsigned long count) {
    memcpy(to, from, count);
}
