/* The brotli harness, which is OSS-Fuzz's brotli_decode_fuzzer written out in C89 declarations. */
/* Upstream is `c/fuzz/decode_fuzzer.c` in google/brotli, which brotli's release tarball does not
   carry, so this is written here rather than taken. It is the same harness line for line and the
   only differences are ones this tree needs: declarations at the top of a block because that is how
   the C in this repository is written, and the output ceiling, which is explained below.

   What it does is worth reading, because the first version written here did something else and the
   difference is the whole point of replaying somebody else's corpus. The last byte of the input
   picks how the input is fed in: the low three bits of it are the chunk size, and zero means feed
   the whole input at once. So an input whose last byte ends in zero goes through the decoder in one
   piece and an input whose last byte ends in five goes in five bytes at a time, suspending and
   resuming the state machine on every one of them. That is where a streaming decoder's bugs live,
   and it is a path an input has to be selected for, which is exactly what years of fuzzing did to
   this corpus. A harness that feeds the whole input every time replays the corpus through a door
   most of it was not chosen for.

   The output is thrown away on purpose. A decompressor's answer is not what a replay is for: the
   monitor is watching every access the decoder makes on the way to producing it, and an input from
   a fuzzing corpus is far more likely to be malformed than to decompress to anything. What matters
   is that every byte read and written on the way is one the model permits. */
#include <brotli/decode.h>
#include <stdlib.h>

/* Upstream's buffer, which is small so that the decoder has to ask for room over and over. */
#define ROOM 1024

/* How much one input may decompress to before the loop gives up.

   Upstream stops at sixty four megabytes when the whole input goes in at once and at sixteen when
   it goes in a few bytes at a time, on the reasoning that the biggest magic number in the format is
   sixteen megabytes less sixteen so nothing longer is interesting. This stops at one megabyte on
   both paths, and that is a deviation to write down rather than to hide. Under instrumentation an
   input costs roughly what it produces, and at upstream's ceilings this corpus averaged almost six
   seconds an input, which is seven hours for one project. What is given up is an input that only
   misbehaves after its first megabyte of output. What is kept is every path the chunking reaches,
   which is what the last byte of the input selects and what this corpus was collected for. */
#define CEILING (1024 * 1024)

int LLVMFuzzerTestOneInput(const unsigned char *data, unsigned long size) {
    const unsigned char *next = data;
    BrotliDecoderState *state;
    unsigned char *room;
    unsigned long addend = 0;
    unsigned long made = 0;
    unsigned long at;

    if (size > 0) {
        addend = data[size - 1] & 7;
    }
    room = malloc(ROOM);
    if (room == NULL) {
        return 0;
    }
    state = BrotliDecoderCreateInstance(NULL, NULL, NULL);
    if (state == NULL) {
        free(room);
        return 0;
    }
    /* Zero means the whole input in one piece, which is the fast path. Anything else is one to
       seven bytes at a time, which is the slow one. */
    if (addend == 0) {
        addend = size;
    }
    for (at = 0; at < size;) {
        unsigned long stop = at + addend;
        unsigned long left;
        BrotliDecoderResult result = BROTLI_DECODER_RESULT_NEEDS_MORE_OUTPUT;

        if (stop > size) {
            stop = size;
        }
        left = stop - at;
        at = stop;
        while (result == BROTLI_DECODER_RESULT_NEEDS_MORE_OUTPUT) {
            unsigned char *out = room;
            unsigned long space = ROOM;

            result =
                BrotliDecoderDecompressStream(state, &left, &next, &space, &out, &made);
            if (made > CEILING) {
                break;
            }
        }
        if (made > CEILING) {
            break;
        }
        /* Anything other than a request for more input is the end of this input, whether that is
           the end of the stream or an error. */
        if (result != BROTLI_DECODER_RESULT_NEEDS_MORE_INPUT) {
            break;
        }
    }
    BrotliDecoderDestroyInstance(state);
    free(room);
    return 0;
}
