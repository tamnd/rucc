// eight words indexed by constants and mixed in a loop, the shape of a salsa20 core

int printf(const char *, ...);

static volatile unsigned seed_in = 5;

int main(void) {
    unsigned seed = seed_in;
    unsigned x[8];
    x[0] = seed;
    x[1] = seed + 1;
    x[2] = seed + 2;
    x[3] = seed + 3;
    x[4] = seed + 4;
    x[5] = seed + 5;
    x[6] = seed + 6;
    x[7] = seed + 7;
    for (unsigned r = 0; r < seed; r++) {
        x[0] += x[4];
        x[1] ^= x[5];
        x[2] += x[6];
        x[3] ^= x[7];
        x[4] = (x[4] << 7) | (x[4] >> 25);
        x[5] = (x[5] << 9) | (x[5] >> 23);
        x[6] += x[0];
        x[7] ^= x[1];
    }
    printf("%u\n", x[0] ^ x[1] ^ x[2] ^ x[3] ^ x[4] ^ x[5] ^ x[6] ^ x[7]);
    return 0;
}
