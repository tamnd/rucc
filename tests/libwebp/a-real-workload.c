/* A workload against an instrumented libwebp, with answers it has to get right. */
/* The sixth library run under the monitor end to end, and the first one whose job is pictures. The
   five before it move bytes around: a compressor reads a buffer and writes a smaller one, a database
   reads a page and writes a row back. This one reads a rectangle. Nearly every file under src/dsp is
   one loop over sixteen pixels written four or five times over, once in plain C and once per
   instruction set the processor might have, and the one that runs is picked at startup by asking the
   processor what it is. That is two things the table has not had: an array indexed by two numbers
   that are multiplied by a stride the caller chose, and a table of function pointers filled in once
   and read from everywhere after. The second is why this row was worth adding at all, since a
   monitor that cannot follow a call through a pointer it watched being stored cannot say anything
   about a library built this way.

   The answers are two checksums this program computes itself over an image it builds itself, so they
   are the same numbers against any version of libwebp and against none. The encoded sizes are not
   checked, for the reason the zlib and brotli workloads give: they are a property of the version and
   the quality, and a bump of the library would turn into a failure here.

   The lossy round trip is not compared byte for byte, because a lossy codec is not required to give
   the same pixels back and the whole point of it is that it does not. What is checked there is that
   it encodes, that the header it wrote says the size that went in, that it decodes, and that the
   average pixel is somewhere near where it started. The lossless round trip and the incremental
   decode are compared byte for byte, since those two are required to be exact.

   The headers are included rather than written out because the decoder is handed an enum value by
   name and the incremental interface takes a state object the library allocates, which is not a
   shape worth writing out by hand. They come from the same tree the sources do. */
#include <stdio.h>
#include <string.h>

#include <webp/decode.h>
#include <webp/encode.h>

/* Ten macroblocks across and eight down, since the encoder decides per macroblock what to do and an
   image one macroblock wide is an image it makes one decision about. */
#define WIDTH 160
#define HEIGHT 128
#define STRIDE (WIDTH * 3)
#define PIXELS (WIDTH * HEIGHT)
#define SOURCE (PIXELS * 3)

/* Deliberately small, so the incremental decode hands the library a few hundred pieces and every
   return that asks to be called again is taken. */
#define CHUNK 400

static unsigned char source[SOURCE];
static unsigned char back[SOURCE];

/* Three kinds of content in one image, in three bands of equal height. Flat blocks first, which is
   what the encoder is best at and what its block predictor is for. Then a gradient, which is smooth
   but never twice the same. Then noise, which no predictor helps with and which sends the lossy path
   down its own branch. */
static void build(void) {
    unsigned long state = 12345;
    int y;
    int x;

    for (y = 0; y < HEIGHT / 3; y++) {
        for (x = 0; x < WIDTH; x++) {
            unsigned char *pixel = source + y * STRIDE + x * 3;
            unsigned long block = (unsigned long)(y / 16) * 16 + (unsigned long)(x / 16);
            pixel[0] = (unsigned char)(31 + block * 7);
            pixel[1] = (unsigned char)(97 + block * 13);
            pixel[2] = (unsigned char)(200 - block * 5);
        }
    }
    for (y = HEIGHT / 3; y < (HEIGHT / 3) * 2; y++) {
        for (x = 0; x < WIDTH; x++) {
            unsigned char *pixel = source + y * STRIDE + x * 3;
            pixel[0] = (unsigned char)(x * 255 / (WIDTH - 1));
            pixel[1] = (unsigned char)(y * 255 / (HEIGHT - 1));
            pixel[2] = (unsigned char)((x + y) * 127 / (WIDTH + HEIGHT - 2));
        }
    }
    for (y = (HEIGHT / 3) * 2; y < HEIGHT; y++) {
        for (x = 0; x < WIDTH; x++) {
            unsigned char *pixel = source + y * STRIDE + x * 3;
            state = state * 1103515245 + 12345;
            pixel[0] = (unsigned char)(state >> 24);
            state = state * 1103515245 + 12345;
            pixel[1] = (unsigned char)(state >> 24);
            state = state * 1103515245 + 12345;
            pixel[2] = (unsigned char)(state >> 24);
        }
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

/* How far the decoded image is from the one that went in, averaged over every byte of it. A lossy
   codec is allowed to be off, so this is the only thing worth asking of that path, and the bound it
   is checked against is loose enough that a version of the library which tunes its encoder differently
   still passes. */
static unsigned long apart(void) {
    unsigned long sum = 0;
    unsigned long i;
    for (i = 0; i < SOURCE; i++) {
        sum += source[i] > back[i] ? (unsigned long)(source[i] - back[i])
                                   : (unsigned long)(back[i] - source[i]);
    }
    return sum / SOURCE;
}

/* One whole-image lossy round trip at one quality, which is the interface most callers use. */
static int lossy(int quality) {
    unsigned char *out = 0;
    size_t size;
    int width = 0;
    int height = 0;

    size = WebPEncodeRGB(source, WIDTH, HEIGHT, STRIDE, (float)quality, &out);
    if (size == 0 || out == 0) return 1;
    if (!WebPGetInfo(out, size, &width, &height)) {
        WebPFree(out);
        return 2;
    }
    if (width != WIDTH || height != HEIGHT) {
        WebPFree(out);
        return 3;
    }
    memset(back, 0, sizeof back);
    if (WebPDecodeRGBInto(out, size, back, sizeof back, STRIDE) == 0) {
        WebPFree(out);
        return 4;
    }
    WebPFree(out);
    if (apart() > 32) return 5;
    return 0;
}

/* The same trip taken losslessly, which is required to give back exactly what went in. */
static int lossless(void) {
    unsigned char *out = 0;
    size_t size;

    size = WebPEncodeLosslessRGB(source, WIDTH, HEIGHT, STRIDE, &out);
    if (size == 0 || out == 0) return 10;
    memset(back, 0, sizeof back);
    if (WebPDecodeRGBInto(out, size, back, sizeof back, STRIDE) == 0) {
        WebPFree(out);
        return 11;
    }
    WebPFree(out);
    if (memcmp(back, source, SOURCE) != 0) return 12;
    return 0;
}

/* A decode fed four hundred bytes at a time, which is the path a caller takes when it is reading
   from a socket and the one where the library holds its state across calls. Lossless again, so the
   answer is still exact, and the bytes are checked against the same image rather than against what
   the whole-image decode produced, since two paths agreeing with each other is not the same as
   either of them being right. */
static int incremental(void) {
    unsigned char *out = 0;
    size_t size;
    WebPIDecoder *reading;
    size_t fed = 0;
    VP8StatusCode status = VP8_STATUS_SUSPENDED;

    size = WebPEncodeLosslessRGB(source, WIDTH, HEIGHT, STRIDE, &out);
    if (size == 0 || out == 0) return 20;
    memset(back, 0, sizeof back);
    reading = WebPINewRGB(MODE_RGB, back, sizeof back, STRIDE);
    if (reading == 0) {
        WebPFree(out);
        return 21;
    }
    while (fed < size) {
        size_t take = size - fed;
        if (take > CHUNK) take = CHUNK;
        status = WebPIAppend(reading, out + fed, take);
        fed += take;
        if (status != VP8_STATUS_OK && status != VP8_STATUS_SUSPENDED) break;
        if (status == VP8_STATUS_OK) break;
    }
    WebPIDelete(reading);
    WebPFree(out);
    if (status != VP8_STATUS_OK) return 22;
    if (memcmp(back, source, SOURCE) != 0) return 23;
    return 0;
}

int main(void) {
    int qualities[3];
    int exact;
    int piecemeal;
    int bad = 0;
    int i;
    unsigned long up;
    unsigned long down;

    build();
    up = forwards(source, SOURCE);
    down = backwards(source, SOURCE);

    qualities[0] = lossy(40);
    qualities[1] = lossy(75);
    qualities[2] = lossy(95);
    for (i = 0; i < 3; i++) {
        if (qualities[i] != 0) bad = 1;
    }
    exact = lossless();
    if (exact != 0) bad = 1;
    piecemeal = incremental();
    if (piecemeal != 0) bad = 1;

    printf("up=%lu down=%lu lossy=%d,%d,%d lossless=%d incremental=%d\n", up, down, qualities[0],
           qualities[1], qualities[2], exact, piecemeal);
    /* The image this program built for itself, checksummed by arithmetic this program does, so these
       are the same two numbers against every version of the library. */
    if (up != 356227197UL) bad = 1;
    if (down != 869191820UL) bad = 1;
    printf("%s\n", bad ? "MISMATCH" : "all answers correct");
    return bad;
}
