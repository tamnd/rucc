/* The zlib harness, which is OSS-Fuzz's zlib_uncompress_fuzzer written out in C. */
/* Upstream is `projects/zlib/zlib_uncompress_fuzzer.cc` in google/oss-fuzz, which is C++ only in
   the sense that it says `extern "C"` and uses a cast, so this is the same twenty lines with the
   two C++ spellings removed. zlib's own tarball carries no fuzz targets, which is why the target
   lives in the OSS-Fuzz repository rather than in the library.

   It is as small as a harness gets: one static buffer, one call to `uncompress`, and the answer
   thrown away. That is not a weakness of the target, it is the shape of the thing being tested.
   `uncompress` is the whole of the one shot decompression path, so an input that gets an inflate
   bug to happen gets it to happen here, and years of fuzzing have selected a corpus of inputs that
   do. The monitor watches every access inflate makes on the way through.

   A quarter of a megabyte of output, which is upstream's number, and it needs no ceiling of its own
   because the buffer is the ceiling: `uncompress` stops with Z_BUF_ERROR the moment the answer does
   not fit, so no input can run long. The buffer is static and zero initialised for the same reason
   it is upstream, which is that a fuzz target should not spend its time in the allocator. That does
   mean the init plane has nothing to say about it, which is worth knowing when reading a quiet
   result: the reads this harness itself makes are of storage that starts out written. Everything
   inside inflate is the library's own and is watched the usual way. */
#include <zlib.h>

/* Upstream's buffer, in the file scope it has upstream. */
static Bytef buffer[256 * 1024];

int LLVMFuzzerTestOneInput(const unsigned char *data, unsigned long size) {
    uLongf room = sizeof buffer;

    uncompress(buffer, &room, (const Bytef *)data, (uLong)size);
    return 0;
}
