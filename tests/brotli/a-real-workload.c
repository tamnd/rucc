/* A workload against an instrumented brotli, with answers it has to get right. */
/* The fourth library run under the monitor end to end, and the second compressor, which wants a
   word of justification since zlib is already a row. They are the same job and not the same code.
   zlib is fifteen small files around one sliding window and a hash chain. brotli carries a static
   dictionary of a hundred and twenty thousand words, splits its input into blocks and picks a
   different set of Huffman tables per block, keeps its decoder state in a ring buffer it grows as
   it learns how much it needs, and reaches back into that ring with an offset that is allowed to
   point at the dictionary instead. That last part is the interesting one here: a distance in a
   brotli stream is a number that selects storage from one of several places, and the decoder works
   out which by arithmetic rather than by holding a pointer to it. Nothing else in the table does
   that.

   The answers are a checksum this program computes itself over a buffer it builds itself, so they
   are the same numbers against any version of brotli and against none. The compressed sizes are not
   checked, for the reason the zlib workload gives: they are a property of the version and the
   quality and would turn a bump of the library into a failure here.

   The headers are included rather than written out because the encoder and the decoder are handed
   enum values by name and the stream functions take a pointer to a pointer the library moves along,
   which is not a shape worth writing out by hand. They come from the same tree the sources do. */
#include <stdio.h>
#include <string.h>

#include <brotli/decode.h>
#include <brotli/encode.h>

/* Small next to the zlib workload's four hundred kilobytes, because brotli at quality 9 does
   considerably more work per byte, and still large enough that the encoder splits the input into
   several blocks and the decoder grows its ring buffer more than once. */
#define SOURCE 120000

/* Deliberately small, so the streaming round trip hands the encoder and the decoder a few hundred
   pieces each and every return path that asks to be called again is taken. */
#define CHUNK 700
#define SPACE 300

static unsigned char source[SOURCE];
static unsigned char packed[SOURCE * 2];
static unsigned char back[SOURCE];

/* Three kinds of content in one buffer, because brotli decides per block what to do and a buffer of
   one kind is a buffer it makes one decision about. Words that its static dictionary already knows,
   then words it does not, then bytes that do not compress at all. */
static void build(void) {
    static const char *known[] = {"the ", "and ", "for ",  "that ", "with ", "this ",
                                  "from ", "have ", "not ", "are ",  "was ",  "http://"};
    static const char *made_up[] = {"quangle ", "wibbet ", "frumious ", "bandersnatch ",
                                    "slithy ",  "tove ",   "mimsy ",    "borogove "};
    unsigned long state = 12345;
    unsigned long at = 0;

    while (at < SOURCE / 3) {
        const char *word = known[(state >> 16) % 12];
        unsigned long len = strlen(word);
        state = state * 1103515245 + 12345;
        if (at + len >= SOURCE / 3) break;
        memcpy(source + at, word, len);
        at += len;
    }
    while (at < SOURCE / 3) source[at++] = ' ';
    while (at < (SOURCE / 3) * 2) {
        const char *word = made_up[(state >> 16) % 8];
        unsigned long len = strlen(word);
        state = state * 1103515245 + 12345;
        if (at + len >= (SOURCE / 3) * 2) break;
        memcpy(source + at, word, len);
        at += len;
    }
    while (at < (SOURCE / 3) * 2) source[at++] = ' ';
    while (at < SOURCE) {
        state = state * 1103515245 + 12345;
        source[at++] = (unsigned char)(state >> 24);
    }
}

/* This program's own arithmetic over a buffer, so the answers belong to the workload rather than to
   the library. Two of them, taken in different directions, so that a run which comes back with the
   right bytes in the wrong order is still wrong. */
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

/* One whole-buffer round trip at one quality, which is the interface most callers use. */
static int whole(int quality) {
    size_t out = sizeof packed;
    size_t got = SOURCE;
    if (!BrotliEncoderCompress(quality, BROTLI_DEFAULT_WINDOW, BROTLI_MODE_GENERIC, SOURCE, source,
                               &out, packed)) {
        return 1;
    }
    if (BrotliDecoderDecompress(out, packed, &got, back) != BROTLI_DECODER_RESULT_SUCCESS) return 2;
    if (got != SOURCE) return 3;
    if (memcmp(back, source, SOURCE) != 0) return 4;
    return 0;
}

/* The same trip taken seven hundred bytes at a time through the streaming interface, with an output
   buffer too small to hold what one call produces, which is the path a real caller takes when it is
   reading from a socket and the path where the library holds state across calls. */
static int streamed(void) {
    BrotliEncoderState *writing = BrotliEncoderCreateInstance(0, 0, 0);
    BrotliDecoderState *reading;
    unsigned char space[SPACE];
    const unsigned char *next_in;
    unsigned char *next_out;
    size_t available_in;
    size_t available_out;
    unsigned long at = 0;
    unsigned long held = 0;
    int result;

    if (!writing) return 10;
    if (!BrotliEncoderSetParameter(writing, BROTLI_PARAM_QUALITY, 5)) return 11;
    if (!BrotliEncoderSetParameter(writing, BROTLI_PARAM_SIZE_HINT, SOURCE)) return 12;
    while (!BrotliEncoderIsFinished(writing)) {
        unsigned long take = SOURCE - at;
        BrotliEncoderOperation op = BROTLI_OPERATION_PROCESS;
        if (take > CHUNK) take = CHUNK;
        if (at + take == SOURCE) op = BROTLI_OPERATION_FINISH;
        next_in = source + at;
        available_in = take;
        next_out = space;
        available_out = SPACE;
        if (!BrotliEncoderCompressStream(writing, op, &available_in, &next_in, &available_out,
                                         &next_out, 0)) {
            BrotliEncoderDestroyInstance(writing);
            return 13;
        }
        at += take - available_in;
        if (held + (SPACE - available_out) > sizeof packed) {
            BrotliEncoderDestroyInstance(writing);
            return 14;
        }
        memcpy(packed + held, space, SPACE - available_out);
        held += SPACE - available_out;
    }
    BrotliEncoderDestroyInstance(writing);

    reading = BrotliDecoderCreateInstance(0, 0, 0);
    if (!reading) return 15;
    at = 0;
    {
        unsigned long fed = 0;
        unsigned long wrote = 0;
        result = BROTLI_DECODER_RESULT_NEEDS_MORE_INPUT;
        while (result != BROTLI_DECODER_RESULT_SUCCESS) {
            unsigned long take = held - fed;
            if (take > CHUNK) take = CHUNK;
            if (take == 0 && result == BROTLI_DECODER_RESULT_NEEDS_MORE_INPUT) break;
            next_in = packed + fed;
            available_in = take;
            fed += take;
            do {
                next_out = space;
                available_out = SPACE;
                result = BrotliDecoderDecompressStream(reading, &available_in, &next_in,
                                                       &available_out, &next_out, 0);
                if (result == BROTLI_DECODER_RESULT_ERROR) {
                    BrotliDecoderDestroyInstance(reading);
                    return 16;
                }
                if (wrote + (SPACE - available_out) > SOURCE) {
                    BrotliDecoderDestroyInstance(reading);
                    return 17;
                }
                memcpy(back + wrote, space, SPACE - available_out);
                wrote += SPACE - available_out;
            } while (available_out == 0 || available_in > 0);
        }
        BrotliDecoderDestroyInstance(reading);
        if (result != BROTLI_DECODER_RESULT_SUCCESS) return 18;
        if (wrote != SOURCE) return 19;
    }
    if (memcmp(back, source, SOURCE) != 0) return 20;
    return 0;
}

int main(void) {
    int qualities[3];
    int bad = 0;
    int i;
    unsigned long up;
    unsigned long down;

    build();
    up = forwards(source, SOURCE);
    down = backwards(source, SOURCE);

    qualities[0] = whole(2);
    qualities[1] = whole(5);
    qualities[2] = whole(9);
    for (i = 0; i < 3; i++) {
        if (qualities[i] != 0) bad = 1;
    }
    if (streamed() != 0) bad = 1;

    printf("up=%lu down=%lu whole=%d,%d,%d\n", up, down, qualities[0], qualities[1], qualities[2]);
    /* The buffer this program built for itself, checksummed by arithmetic this program does, so
       these are the same two numbers against every version of the library. */
    if (up != 1325542071UL) bad = 1;
    if (down != 221950633UL) bad = 1;
    printf("%s\n", bad ? "MISMATCH" : "all answers correct");
    return bad;
}
