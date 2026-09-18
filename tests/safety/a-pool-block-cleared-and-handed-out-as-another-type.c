/* row: S4 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
void *memset(void *p, int byte, unsigned long size);
/* The same reuse the subobject row refuses, with a `memset` in between, which is what makes it
   allowed rather than refused. C 6.5 says storage written through a character type is storage a
   later access may give whatever type it likes, and `memset` writes bytes, so clearing a block is
   how a pool allocator says the old occupant is gone. The loop a program writes out by hand has
   always been read that way here because the compiler instruments each of its stores. `memset` goes
   through a wrapper instead, and the wrapper used to record what it wrote on the init plane and say
   nothing at all on the type plane, so the block kept the type of whoever had it last and the next
   read of it was refused. Every pool allocator that clears a block before reusing it walked into
   that, which is most of them. */
struct counter {
    long total;
};
struct weight {
    int whole;
    int part;
};

int main(void) {
    void *block = malloc(64);
    struct counter *first = block;
    struct weight *second;
    int answer;

    first->total = 7;
    memset(block, 0, 64);

    second = block;
    answer = second->whole + second->part;
    free(block);
    return answer;
}
