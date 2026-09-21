/* The zstd harness, which is OSS-Fuzz's simple_decompress target in one file. */
/* Upstream is `tests/fuzz/simple_decompress.c` in facebook/zstd, and unlike every other row in
   this table zstd ships its fuzz targets in the release tarball, so this could have been compiled
   from there. It is written out here anyway, because upstream's target is three files rather than
   one: the target itself, `fuzz_data_producer.c` and the part of `fuzz_helpers.c` it calls, and
   `xtask replay` compiles one harness per project by design. Splitting that design so that one row
   can name three files would make every other row carry a list of one.

   What the target does is the thing worth reading before reading a result. The input is not a zstd
   frame. It is a zstd frame with a few parameter bytes on the end, and the first thing the target
   does is take a random number of bytes off the back for its own use and hand the rest to the
   decompressor. Those bytes decide the size of the output buffer, which is anywhere from zero to
   ten times the length of the frame, so the same input run twice runs twice the same way and a
   corpus collected this way is a corpus of frames paired with buffer sizes. That is why the buffer
   is sometimes far too small: a decompressor being asked to write into a buffer that cannot hold
   the answer is half of what this target is for.

   The other half is the size agreement at the end. When a decompression succeeds, the size the
   frame header claims and the number of bytes that came out have to be the same number, unless the
   header claims not to know, and a mismatch is an assertion failure rather than a return. Upstream
   makes that check because a decompressor that writes the right bytes and the wrong count is a
   decompressor whose callers overrun something later, and it is kept here for the same reason.

   The context is created once and freed at the end of every input rather than kept, which is what
   upstream does when `STATEFUL_FUZZING` is not defined, and it is not defined here. Keeping a
   context across inputs is a way to find bugs in the reset path, and it is also a way to make one
   input's report depend on the input before it, which is the opposite of what a replay wants. */
#define ZSTD_STATIC_LINKING_ONLY

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#include "zstd.h"

/* Upstream's assertion, which is on in every build because a fuzz target has no other way to say
   that something it expected did not hold. */
#define ZAT(cond)                                                                                  \
    ((cond) ? (void)0                                                                              \
            : (fprintf(stderr, "zstd harness: %u: %s\n", __LINE__, #cond), abort()))

/* The state the parameter bytes are taken from, which is upstream's `FUZZ_dataProducer_t`.

   It reads from the end of the input and works backwards, so that the bytes it takes are the ones
   furthest from the frame header, and when it runs out it keeps answering the bottom of whatever
   range it was asked for. */
struct producer {
    const uint8_t *data;
    size_t size;
};

/* A number in [min, max], built out of as many bytes off the back of the input as the range needs.

   This is upstream's arithmetic unchanged, including the modulo, which is biased and does not
   matter: the input is chosen by a fuzzer rather than drawn from a distribution. */
static uint32_t between(struct producer *from, uint32_t min, uint32_t max) {
    uint32_t range = max - min;
    uint32_t rolling = range;
    uint32_t result = 0;

    ZAT(min <= max);
    while (rolling > 0 && from->size > 0) {
        uint8_t next = *(from->data + from->size - 1);

        from->size -= 1;
        result = (result << 8) | next;
        rolling >>= 8;
    }
    if (range == 0xffffffff) {
        return result;
    }
    return min + result % (range + 1);
}

/* Keeps the last `keep` bytes and hands back how many are no longer the producer's. */
static size_t contract(struct producer *from, size_t keep) {
    size_t effective = keep > from->size ? from->size : keep;
    size_t remaining = from->size - effective;

    from->data = from->data + remaining;
    from->size = effective;
    return remaining;
}

/* Splits the input into the frame and the parameter bytes, and hands back the length of the frame.

   The split point is itself read out of the input, so where the frame ends is one more thing the
   fuzzer chose. */
static size_t reserve(struct producer *from) {
    size_t slice = between(from, 0, (uint32_t)from->size);

    return contract(from, slice);
}

/* Upstream's `FUZZ_malloc`, which answers a null pointer for no bytes rather than whatever the
   allocator would, since the two are not the same thing to a caller that goes on to check. */
static void *taken(size_t size) {
    if (size > 0) {
        void *const mem = malloc(size);

        ZAT(mem);
        return mem;
    }
    return NULL;
}

int LLVMFuzzerTestOneInput(const uint8_t *src, size_t size);

int LLVMFuzzerTestOneInput(const uint8_t *src, size_t size) {
    struct producer from;
    ZSTD_DCtx *context = NULL;
    size_t room;
    void *out;
    size_t wrote;

    from.data = src;
    from.size = size;
    size = reserve(&from);

    context = ZSTD_createDCtx();
    ZAT(context);

    room = between(&from, 0, (uint32_t)(10 * size));
    out = taken(room);
    wrote = ZSTD_decompressDCtx(context, out, room, src, size);
    if (!ZSTD_isError(wrote)) {
        /* A frame that decompressed has a header, and the header either says how many bytes were
           in it or says it does not know. Anything else is the decompressor and the header
           disagreeing about what just happened. */
        unsigned long long const expected = ZSTD_findDecompressedSize(src, size);

        ZAT(expected != ZSTD_CONTENTSIZE_ERROR);
        ZAT(expected == ZSTD_CONTENTSIZE_UNKNOWN || expected == wrote);
    }
    free(out);
    ZSTD_freeDCtx(context);
    return 0;
}
