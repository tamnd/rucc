// The frame_fn of the rucc-corpus case frame-size.8.16384.4.leaf, which sums an array of 16384
// words while four structures keep most of the registers busy, and the main of the cases
// call-motion.*.a-call-under-a-false-test, whose sum leaves the loop for a call that wants it in
// another register. tamnd/rucc#3187.
int printf(const char *, ...);
long long divide(long long top, long long bottom);
extern volatile long long seed_in, gate_in, bottom_in;

struct record {
    unsigned long long key;
    unsigned int vals[6];
    unsigned short tag;
    unsigned char name[21];
};

unsigned long long frame_fn(unsigned long long seed, int depth) {
    unsigned long long l0 = seed * 3 + 1;
    unsigned long long l1 = l0 * 5 + 1;
    unsigned long long l2 = l1 * 7 + 2;
    unsigned long long l3 = l2 * 9 + 3;
    unsigned long long l4 = l3 * 11 + 4;
    unsigned long long l5 = l4 * 13 + 5;
    unsigned long long l6 = l5 * 15 + 6;
    unsigned long long l7 = l6 * 17 + 7;
    unsigned int arr[16384];
    for (int i = 0; i < 16384; i++) {
        arr[i] = (unsigned int)(seed + (unsigned long long)i * 2654435761u);
    }
    struct record r0;
    r0.key = l0 + 0;
    for (int i = 0; i < 6; i++) {
        r0.vals[i] = (unsigned int)(l1 + 7 * (unsigned long long)i);
    }
    r0.tag = 1;
    for (int i = 0; i < 21; i++) {
        r0.name[i] = (unsigned char)(seed + i * 3);
    }
    struct record r1;
    r1.key = l1 + 1;
    for (int i = 0; i < 6; i++) {
        r1.vals[i] = (unsigned int)(l2 + 7 * (unsigned long long)i);
    }
    r1.tag = 2;
    for (int i = 0; i < 21; i++) {
        r1.name[i] = (unsigned char)(seed + i * 4);
    }
    struct record r2;
    r2.key = l2 + 2;
    for (int i = 0; i < 6; i++) {
        r2.vals[i] = (unsigned int)(l3 + 7 * (unsigned long long)i);
    }
    r2.tag = 3;
    for (int i = 0; i < 21; i++) {
        r2.name[i] = (unsigned char)(seed + i * 5);
    }
    struct record r3;
    r3.key = l3 + 3;
    for (int i = 0; i < 6; i++) {
        r3.vals[i] = (unsigned int)(l4 + 7 * (unsigned long long)i);
    }
    r3.tag = 4;
    for (int i = 0; i < 21; i++) {
        r3.name[i] = (unsigned char)(seed + i * 6);
    }
    unsigned long long acc = 0;
    unsigned long long words = 0;
    for (int i = 0; i < 16384; i++) {
        words = words * 33 + arr[i];
    }
    acc = acc + words;
    r0.tag = (unsigned short)(r0.tag + 1);
    unsigned long long mix0 = r0.key;
    for (int i = 0; i < 6; i++) {
        mix0 = mix0 * 7 + r0.vals[i];
    }
    for (int i = 0; i < 21; i++) {
        mix0 = mix0 + r0.name[i];
    }
    acc = acc + mix0 + r0.tag;
    r1.tag = (unsigned short)(r1.tag + 1);
    unsigned long long mix1 = r1.key;
    for (int i = 0; i < 6; i++) {
        mix1 = mix1 * 7 + r1.vals[i];
    }
    for (int i = 0; i < 21; i++) {
        mix1 = mix1 + r1.name[i];
    }
    acc = acc + mix1 + r1.tag;
    r2.tag = (unsigned short)(r2.tag + 1);
    unsigned long long mix2 = r2.key;
    for (int i = 0; i < 6; i++) {
        mix2 = mix2 * 7 + r2.vals[i];
    }
    for (int i = 0; i < 21; i++) {
        mix2 = mix2 + r2.name[i];
    }
    acc = acc + mix2 + r2.tag;
    r3.tag = (unsigned short)(r3.tag + 1);
    unsigned long long mix3 = r3.key;
    for (int i = 0; i < 6; i++) {
        mix3 = mix3 * 7 + r3.vals[i];
    }
    for (int i = 0; i < 21; i++) {
        mix3 = mix3 + r3.name[i];
    }
    acc = acc + mix3 + r3.tag;
    acc = acc * 3 + l0;
    (void)depth;
    acc = acc * 31 + l7;
    acc = acc * 31 + l6;
    acc = acc * 31 + l5;
    acc = acc * 31 + l4;
    acc = acc * 31 + l3;
    acc = acc * 31 + l2;
    acc = acc * 31 + l1;
    acc = acc * 31 + l0;
    acc = acc + arr[16383] + arr[8192];
    acc = acc * 31 + r0.key + r0.tag + r0.vals[5] + r0.name[20];
    acc = acc * 31 + r1.key + r1.tag + r1.vals[5] + r1.name[20];
    acc = acc * 31 + r2.key + r2.tag + r2.vals[5] + r2.name[20];
    acc = acc * 31 + r3.key + r3.tag + r3.vals[5] + r3.name[20];
    return acc;
}

void gated(void) {
    long long seed = seed_in;
    long long gate = gate_in;
    long long bottom = bottom_in;
    long long total = 0;
    for (int i = 0; i < 1000; i++) {
        if (gate) {
            total += divide(seed, bottom);
        }
        total += 1;
    }
    printf("%lld\n", total);
}
