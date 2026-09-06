/* Document 12 section 12.4's third addition. */
/* A bucket array of chains, which is a random access into a large array followed by a short
   pointer walk. Neither half is predictable, so this is the row where a check that costs a
   branch misprediction shows up rather than one that costs a miss. */
void *malloc(unsigned long size);
void free(void *p);

struct entry {
    struct entry *next;
    long key;
};

#define BUCKETS 4096

int main(void) {
    struct entry **table = malloc(BUCKETS * sizeof(struct entry *));
    long found = 0;
    int i;
    int round;
    for (i = 0; i < BUCKETS; i++) {
        table[i] = 0;
    }
    for (i = 0; i < 100000; i++) {
        long key = (long)i * 2654435761L;
        int bucket = (int)((key >> 16) & (BUCKETS - 1));
        struct entry *fresh = malloc(sizeof(struct entry));
        fresh->key = key;
        fresh->next = table[bucket];
        table[bucket] = fresh;
    }
    for (round = 0; round < 10; round++) {
        for (i = 0; i < 100000; i++) {
            long key = (long)i * 2654435761L;
            int bucket = (int)((key >> 16) & (BUCKETS - 1));
            struct entry *cursor = table[bucket];
            while (cursor) {
                if (cursor->key == key) {
                    found++;
                    break;
                }
                cursor = cursor->next;
            }
        }
    }
    for (i = 0; i < BUCKETS; i++) {
        struct entry *cursor = table[i];
        while (cursor) {
            struct entry *next = cursor->next;
            free(cursor);
            cursor = next;
        }
    }
    free(table);
    return found == 0 ? 1 : 0;
}
