/* accept: all */
/* `BF` is the mode of `__bf16`, the brain float. libgcc's soft-fp `brain.h` declares its type
   this way, and the conversions to and from it read it through a union with an integer of the
   same size. The mode is there where `__bf16` is, which is x86-64 here. */

typedef float __attribute__((mode(BF))) bf;
typedef unsigned int __attribute__((mode(HI))) u16;

union bits {
    bf value;
    u16 word;
};

typedef int width[sizeof(bf) == 2 && sizeof(union bits) == 2 ? 1 : -1];

bf from_bits(u16 word) {
    union bits u;
    u.word = word;
    return u.value;
}

u16 to_bits(bf value) {
    union bits u;
    u.value = value;
    return u.word;
}
