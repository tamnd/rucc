/* A workload against an instrumented zstd, with answers it has to get right. */
/* The fifth library run under the monitor end to end, and the third compressor, which is two more
   than the argument for a table needs unless the third one does something the first two do not. It
   does two things. Its match finders keep several tables of positions at once and switch between
   them by strategy, so the same buffer is indexed by three different schemes in one compression,
   and its dictionary builder runs a suffix array over the samples, which is the only code in the
   table that sorts pointers into a buffer rather than walking them. A suffix array is a hundred
   thousand comparisons between two addresses into the same object, which is a thing a capability
   model has to get exactly right and which nothing else here asks for.

   What running it found is bigger than either. zstd keeps every position as a 32 bit index and one
   pointer that turns an index into an address, and it computes that pointer as the caller's buffer
   moved backwards by everything the compressor has seen, so it points a long way before the start
   of any object. It is never dereferenced and every match finder reads through it at an offset that
   lands back inside the buffer it came from. The model refuses the derivation and clears the
   capability, so all four and a half million of those reads are refused as well, which is
   tamnd/rucc#1417 and is why this row's reports are not held to a list yet.

   The answers are a checksum this program computes itself over a buffer it builds itself, so they
   are the same numbers against any version of zstd and against none. The compressed sizes are not
   checked, for the reason the zlib workload gives: they are a property of the version and the level
   and would turn a bump of the library into a failure here.

   The headers are included rather than written out because the streaming interface passes structures
   by value that hold a pointer, a capacity and a position the library moves along, and their layout
   has to be the one the library was compiled with. They come from the same tree the sources do. */
#include <stdio.h>
#include <string.h>

#include <zdict.h>
#include <zstd.h>

/* Large enough that the match finders go round their tables more than once and the block splitter
   has several blocks to make a decision about, small enough that level 12 under the monitor is
   still a few seconds. */
#define SOURCE 200000

/* Deliberately small, so the streaming round trip hands the library a few hundred pieces and every
   return path that asks to be called again is taken. */
#define CHUNK 800
#define SPACE 300

/* The dictionary is trained from many small samples, because that is what a dictionary is for and
   because it is what makes the builder sort a real suffix array rather than a trivial one. */
#define SAMPLES 300
#define SAMPLE 400
#define DICTIONARY 8192

static unsigned char source[SOURCE];
static unsigned char packed[SOURCE * 2];
static unsigned char back[SOURCE];
static unsigned char dictionary[DICTIONARY];

/* Half text with a small vocabulary, which is what a dictionary can do something with, and half a
   linear congruential stream, which nothing can, so both the match path and the literal path carry
   real traffic. */
static void build(void) {
    static const char *words[] = {"alpha ", "bravo ",  "charlie ", "delta ", "echo ",   "foxtrot ",
                                  "golf ",  "hotel ",  "india ",   "juliet ", "kilo ",  "lima ",
                                  "mike ",  "november ", "oscar ", "papa "};
    unsigned long state = 12345;
    unsigned long at = 0;

    while (at < SOURCE / 2) {
        const char *word = words[(state >> 16) % 16];
        unsigned long len = strlen(word);
        state = state * 1103515245 + 12345;
        if (at + len >= SOURCE / 2) break;
        memcpy(source + at, word, len);
        at += len;
    }
    while (at < SOURCE / 2) source[at++] = ' ';
    while (at < SOURCE) {
        state = state * 1103515245 + 12345;
        source[at++] = (unsigned char)(state >> 24);
    }
}

/* This program's own arithmetic over a buffer, so the answers belong to the workload rather than to
   the library. Two of them, taken in opposite directions, so a run that comes back with the right
   bytes in the wrong order is still wrong. */
static unsigned long forwards(const unsigned char *bytes, unsigned long len) {
    unsigned long sum = 2166136261UL;
    unsigned long i;
    for (i = 0; i < len; i++) sum = ((sum ^ bytes[i]) * 16777619UL) & 0xffffffffUL;
    return sum;
}

static unsigned long backwards(const unsigned char *bytes, unsigned long len) {
    unsigned long sum = 0;
    unsigned long i;
    for (i = len; i > 0; i--) sum = (sum * 31 + bytes[i - 1]) % 1000000007UL;
    return sum;
}

/* One whole-buffer round trip at one level, which is the interface most callers use. The three
   levels reached below are three different match finders rather than three settings of one. */
static int whole(int level) {
    size_t out = ZSTD_compress(packed, sizeof packed, source, SOURCE, level);
    size_t got;
    if (ZSTD_isError(out)) return 1;
    got = ZSTD_decompress(back, SOURCE, packed, out);
    if (ZSTD_isError(got)) return 2;
    if (got != SOURCE) return 3;
    if (memcmp(back, source, SOURCE) != 0) return 4;
    return 0;
}

/* The same trip taken eight hundred bytes at a time, with an output buffer too small to hold what
   one call produces, which is the path a caller reading from a socket takes and the one where the
   library holds its window across calls. */
static int streamed(void) {
    ZSTD_CStream *writing = ZSTD_createCStream();
    ZSTD_DStream *reading;
    unsigned char space[SPACE];
    ZSTD_inBuffer in;
    ZSTD_outBuffer out;
    unsigned long at = 0;
    unsigned long held = 0;

    if (!writing) return 10;
    if (ZSTD_isError(ZSTD_initCStream(writing, 5))) return 11;
    for (;;) {
        unsigned long take = SOURCE - at;
        ZSTD_EndDirective op = ZSTD_e_continue;
        size_t left;
        if (take > CHUNK) take = CHUNK;
        if (at + take == SOURCE) op = ZSTD_e_end;
        in.src = source + at;
        in.size = take;
        in.pos = 0;
        do {
            out.dst = space;
            out.size = SPACE;
            out.pos = 0;
            left = ZSTD_compressStream2(writing, &out, &in, op);
            if (ZSTD_isError(left)) {
                ZSTD_freeCStream(writing);
                return 12;
            }
            if (held + out.pos > sizeof packed) {
                ZSTD_freeCStream(writing);
                return 13;
            }
            memcpy(packed + held, space, out.pos);
            held += out.pos;
        } while (in.pos < in.size || (op == ZSTD_e_end && left != 0));
        at += take;
        if (op == ZSTD_e_end) break;
    }
    ZSTD_freeCStream(writing);

    reading = ZSTD_createDStream();
    if (!reading) return 14;
    if (ZSTD_isError(ZSTD_initDStream(reading))) return 15;
    {
        unsigned long fed = 0;
        unsigned long wrote = 0;
        while (fed < held) {
            unsigned long take = held - fed;
            if (take > CHUNK) take = CHUNK;
            in.src = packed + fed;
            in.size = take;
            in.pos = 0;
            fed += take;
            while (in.pos < in.size) {
                size_t status;
                out.dst = space;
                out.size = SPACE;
                out.pos = 0;
                status = ZSTD_decompressStream(reading, &out, &in);
                if (ZSTD_isError(status)) {
                    ZSTD_freeDStream(reading);
                    return 16;
                }
                if (wrote + out.pos > SOURCE) {
                    ZSTD_freeDStream(reading);
                    return 17;
                }
                memcpy(back + wrote, space, out.pos);
                wrote += out.pos;
            }
        }
        ZSTD_freeDStream(reading);
        if (wrote != SOURCE) return 18;
    }
    if (memcmp(back, source, SOURCE) != 0) return 19;
    return 0;
}

/* A dictionary trained from three hundred small samples and then used on a sample the training
   never saw. The training is the suffix array, which is the reason this row exists, and the use is
   the path where the compressor's window starts out pointing at storage the caller owns. */
static int with_a_dictionary(void) {
    static size_t sizes[SAMPLES];
    ZSTD_CCtx *writing;
    ZSTD_DCtx *reading;
    size_t trained;
    size_t out;
    size_t got;
    unsigned long probe = SOURCE / 2 - SAMPLE * 2;
    int i;

    for (i = 0; i < SAMPLES; i++) sizes[i] = SAMPLE;
    trained = ZDICT_trainFromBuffer(dictionary, DICTIONARY, source, sizes, SAMPLES);
    if (ZDICT_isError(trained)) return 20;
    if (trained == 0 || trained > DICTIONARY) return 21;

    writing = ZSTD_createCCtx();
    if (!writing) return 22;
    out = ZSTD_compress_usingDict(writing, packed, sizeof packed, source + probe, SAMPLE, dictionary,
                                  trained, 5);
    ZSTD_freeCCtx(writing);
    if (ZSTD_isError(out)) return 23;

    reading = ZSTD_createDCtx();
    if (!reading) return 24;
    got = ZSTD_decompress_usingDict(reading, back, SOURCE, packed, out, dictionary, trained);
    ZSTD_freeDCtx(reading);
    if (ZSTD_isError(got)) return 25;
    if (got != SAMPLE) return 26;
    if (memcmp(back, source + probe, SAMPLE) != 0) return 27;
    return 0;
}

int main(void) {
    int levels[3];
    int bad = 0;
    int i;
    unsigned long up;
    unsigned long down;

    build();
    up = forwards(source, SOURCE);
    down = backwards(source, SOURCE);

    levels[0] = whole(1);
    levels[1] = whole(5);
    levels[2] = whole(12);
    for (i = 0; i < 3; i++) {
        if (levels[i] != 0) bad = 1;
    }
    if (streamed() != 0) bad = 1;
    if (with_a_dictionary() != 0) bad = 1;

    printf("up=%lu down=%lu whole=%d,%d,%d\n", up, down, levels[0], levels[1], levels[2]);
    /* The buffer this program built for itself, checksummed by arithmetic this program does, so
       these are the same two numbers against every version of the library. */
    if (up != 4168777488UL) bad = 1;
    if (down != 661630325UL) bad = 1;
    printf("%s\n", bad ? "MISMATCH" : "all answers correct");
    return bad;
}
