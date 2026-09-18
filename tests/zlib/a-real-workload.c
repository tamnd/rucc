/* A workload against an instrumented zlib, with answers it has to get right. */
/* The second library run under the monitor end to end, for the reason the SQLite one gives and to
   answer a different question. SQLite is one enormous translation unit that recycles its own
   storage through a lookaside allocator. zlib is fifteen small ones that allocate a few large
   buffers up front, index them with pointers held inside a structure the caller owns, and then
   spend all their time walking those pointers backwards and forwards. The second shape is the one
   that puts pressure on the capability rather than on the type plane, and neither library reaches
   the other's paths.

   The answers are checksums of a buffer this program builds for itself, so they are arithmetic and
   they do not depend on which zlib this is built against. The compressed sizes are not checked for
   the opposite reason: they are a property of the version and the level and would turn a bump of
   the library into a failure here.

   zlib.h is included rather than written out, unlike the SQLite workload's four prototypes,
   because z_stream is a structure this program declares by value and its layout has to be the
   layout the library was compiled with. The header comes from the same tree the sources do. */
#include <stdio.h>
#include <string.h>
#include <zlib.h>

/* Big enough that deflate goes round its window more than once and small enough that the whole run
   takes under a second at every level. */
#define SOURCE 400000

/* Deliberately small, so the streaming round trip goes through deflate and inflate a few thousand
   times each and the state machine is entered in every state it has rather than in the one a
   single call would use. */
#define CHUNK 1000
#define SPACE 400

static unsigned char source[SOURCE];
static unsigned char packed[SOURCE * 2];
static unsigned char back[SOURCE];

/* Half English-looking text, which compresses, and half a linear congruential stream, which does
   not, so that both the literal path and the match path carry real traffic. */
static void build(void) {
    static const char *words[] = {"the", "quick", "brown", "fox", "jumps", "over", "a", "lazy",
                                  "dog", "and", "then", "sleeps"};
    unsigned long state = 12345;
    unsigned long at = 0;
    while (at < SOURCE / 2) {
        const char *word = words[(state >> 16) % 12];
        unsigned long len = strlen(word);
        if (at + len + 1 >= SOURCE / 2) break;
        memcpy(source + at, word, len);
        at += len;
        source[at++] = ' ';
        state = state * 1103515245 + 12345;
    }
    while (at < SOURCE / 2) source[at++] = ' ';
    while (at < SOURCE) {
        state = state * 1103515245 + 12345;
        source[at++] = (unsigned char)(state >> 24);
    }
}

/* One whole-buffer round trip at one level, which is the interface most callers of zlib use. */
static int whole(int level) {
    uLongf out = sizeof packed;
    uLongf got = SOURCE;
    if (compress2(packed, &out, source, SOURCE, level) != Z_OK) return 1;
    if (uncompress(back, &got, packed, out) != Z_OK) return 2;
    if (got != SOURCE) return 3;
    if (memcmp(back, source, SOURCE) != 0) return 4;
    return 0;
}

/* The same trip taken a thousand bytes at a time through the streaming interface, with an output
   buffer too small to hold what one call produces, so every return path that asks to be called
   again is taken. */
static int streamed(void) {
    z_stream out;
    z_stream in;
    unsigned char space[SPACE];
    unsigned long at = 0;
    unsigned long held = 0;
    int done;

    memset(&out, 0, sizeof out);
    if (deflateInit(&out, 6) != Z_OK) return 10;
    do {
        unsigned long take = SOURCE - at;
        if (take > CHUNK) take = CHUNK;
        out.next_in = source + at;
        out.avail_in = (uInt)take;
        at += take;
        done = at == SOURCE;
        do {
            out.next_out = space;
            out.avail_out = SPACE;
            if (deflate(&out, done ? Z_FINISH : Z_NO_FLUSH) == Z_STREAM_ERROR) return 11;
            if (held + (SPACE - out.avail_out) > sizeof packed) return 12;
            memcpy(packed + held, space, SPACE - out.avail_out);
            held += SPACE - out.avail_out;
        } while (out.avail_out == 0);
    } while (!done);
    if (deflateEnd(&out) != Z_OK) return 13;

    memset(&in, 0, sizeof in);
    if (inflateInit(&in) != Z_OK) return 14;
    at = 0;
    {
        unsigned long fed = 0;
        unsigned long wrote = 0;
        int status = Z_OK;
        while (fed < held && status != Z_STREAM_END) {
            unsigned long take = held - fed;
            if (take > CHUNK) take = CHUNK;
            in.next_in = packed + fed;
            in.avail_in = (uInt)take;
            fed += take;
            do {
                in.next_out = space;
                in.avail_out = SPACE;
                status = inflate(&in, Z_NO_FLUSH);
                if (status == Z_STREAM_ERROR || status == Z_DATA_ERROR) return 15;
                if (wrote + (SPACE - in.avail_out) > SOURCE) return 16;
                memcpy(back + wrote, space, SPACE - in.avail_out);
                wrote += SPACE - in.avail_out;
            } while (in.avail_out == 0);
        }
        if (inflateEnd(&in) != Z_OK) return 17;
        if (wrote != SOURCE) return 18;
    }
    if (memcmp(back, source, SOURCE) != 0) return 19;
    return 0;
}

/* The gzip file interface, which is the third of zlib's three surfaces and the only one that holds
   a buffer it grew itself across calls the caller makes one at a time. */
static int through_a_file(const char *path) {
    gzFile writing = gzopen(path, "wb6");
    gzFile reading;
    unsigned long at = 0;
    if (!writing) return 20;
    while (at < SOURCE) {
        unsigned long take = SOURCE - at;
        if (take > CHUNK) take = CHUNK;
        if (gzwrite(writing, source + at, (unsigned)take) != (int)take) return 21;
        at += take;
    }
    if (gzclose(writing) != Z_OK) return 22;

    reading = gzopen(path, "rb");
    if (!reading) return 23;
    at = 0;
    for (;;) {
        int got = gzread(reading, back + at, SPACE);
        if (got < 0) return 24;
        if (got == 0) break;
        at += (unsigned long)got;
        if (at > SOURCE) return 25;
    }
    if (gzclose(reading) != Z_OK) return 26;
    if (at != SOURCE) return 27;
    if (memcmp(back, source, SOURCE) != 0) return 28;
    return 0;
}

int main(void) {
    int levels[3];
    int bad = 0;
    int i;
    unsigned long sum;
    unsigned long red;

    build();
    sum = adler32(adler32(0, Z_NULL, 0), source, SOURCE);
    red = crc32(crc32(0, Z_NULL, 0), source, SOURCE);

    levels[0] = whole(1);
    levels[1] = whole(6);
    levels[2] = whole(9);
    for (i = 0; i < 3; i++) {
        if (levels[i] != 0) bad = 1;
    }
    if (streamed() != 0) bad = 1;
    if (through_a_file("a-real-workload.gz") != 0) bad = 1;

    printf("adler=%lu crc=%lu whole=%d,%d,%d\n", sum, red, levels[0], levels[1], levels[2]);
    /* The buffer this program built for itself, so these are the same two numbers against every
       version of the library and against no library at all. */
    if (sum != 1273253095UL) bad = 1;
    if (red != 1162207700UL) bad = 1;
    printf("%s\n", bad ? "MISMATCH" : "all answers correct");
    return bad;
}
