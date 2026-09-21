/* The brotli harness, standing in for OSS-Fuzz's brotli_decode_fuzzer. */
/* brotli's release tarball does not carry its fuzz targets, so this is written here rather than
   taken. What the upstream target does is hand the input to the decoder and throw the output away,
   once through the one shot entry point and once through the streaming one, because the two take
   different paths through the same decoder and the streaming one is where the state machine lives.
   This does the same two. It is the inputs that are being borrowed, not the twenty lines around
   them, and an input that reaches a bug in the decoder reaches it through either door.

   The output is thrown away on purpose. A decompressor's answer is not what a replay is for: the
   monitor is watching every access the decoder makes on the way to producing it, and an input from
   a fuzzing corpus is far more likely to be malformed than to decompress to anything. What matters
   is that every byte read and written on the way is one the model permits. */
#include <brotli/decode.h>
#include <stdlib.h>

/* One page out at a time. Small enough that a corpus entry which expands enormously does not turn
   the replay into a memory test, and big enough that the streaming loop is not called once per
   byte. */
#define ROOM 4096

/* How much a single input is allowed to decompress to before the streaming loop gives up. A corpus
   collected by a fuzzer is full of small inputs that expand without bound, because that is a thing
   a fuzzer finds and a compression format permits, and upstream does not need a cap because
   libFuzzer kills a target that runs too long.

   A megabyte, which is two hundred and fifty six turns of the loop, and the number was measured
   rather than picked. What a replay wants out of an input is every path through the decoder the
   input reaches, and an input reaches all of them in its first few kilobytes: everything after that
   is the same loop over more of the same data. Sixty four megabytes was the first cap tried and it
   made the average input take almost six seconds under instrumentation, which is seven hours for
   this corpus and a replay nobody runs. A megabyte is the same coverage in a twentieth of the
   time. */
#define CEILING (1024 * 1024)

/* The one shot door, which is what a caller with the whole input in hand uses. */
static void whole(const unsigned char *data, unsigned long size) {
    unsigned long out = ROOM;
    unsigned char *room = malloc(ROOM);

    if (room == NULL) {
        return;
    }
    BrotliDecoderDecompress(size, data, &out, room);
    free(room);
}

/* The streaming door, which is the state machine. The loop stops on anything that is not a request
   for more room, which covers the end of the stream, an error and the decoder asking for more input
   than a replay has, since the whole input went in at the start and there is no more. It also stops
   once the input has produced CEILING bytes, which is the only way out of an input that decompresses
   forever. */
static void piece(const unsigned char *data, unsigned long size) {
    BrotliDecoderState *state = BrotliDecoderCreateInstance(NULL, NULL, NULL);
    unsigned char *room = malloc(ROOM);
    const unsigned char *next = data;
    unsigned long left = size;
    unsigned long made = 0;

    if (state == NULL || room == NULL) {
        free(room);
        if (state != NULL) {
            BrotliDecoderDestroyInstance(state);
        }
        return;
    }
    for (;;) {
        unsigned char *out = room;
        unsigned long space = ROOM;
        BrotliDecoderResult result =
            BrotliDecoderDecompressStream(state, &left, &next, &space, &out, &made);

        if (result != BROTLI_DECODER_RESULT_NEEDS_MORE_OUTPUT || made > CEILING) {
            break;
        }
    }
    free(room);
    BrotliDecoderDestroyInstance(state);
}

int LLVMFuzzerTestOneInput(const unsigned char *data, unsigned long size) {
    whole(data, size);
    piece(data, size);
    return 0;
}
