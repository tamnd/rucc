/* The libjpeg-turbo harness, which is OSS-Fuzz's libjpeg_turbo_fuzzer written out in C. */
/* Upstream is `fuzz/decompress.cc` in the libjpeg-turbo tarball. Its CMakeLists builds it under the
   name libjpeg_turbo_fuzzer rather than decompress_fuzzer, so that the corpus OSS-Fuzz had already
   accumulated under that name stayed with it when the target moved into the library's own tree, and
   that is the name the corpus is published under. It is C++ only in the sense that it says
   `extern "C"` and casts with the C++ spelling of nothing at all, so this is the same function with
   `extern "C"` taken off and the declarations moved to the top of their blocks.

   It decodes each input five times, once into each of five pixel formats, because the colour
   conversion code is different for each and a corpus picked for coverage got there through all
   five. The first pass turns on bottom up output, fast upsampling and the fast DCT, the second and
   third scale the IDCT down by two and by eight, and the second and fourth crop to a fixed region
   when the image is big enough to hold it. The header is read and its answer ignored on purpose,
   because a malformed JPEG that gets an error out of the header can still have something to say
   to the decoder.

   Upstream's two ceilings are kept as they are, a megapixel of output and a hundred scans of a
   progressive image, or fifty of a lossless one. Without them a corpus input that describes a
   picture the size of a field would spend its time allocating rather than decoding, which is what
   the ceilings are there to stop under libFuzzer too.

   The sum over every output sample is upstream's, and it is not decoration. It is there so that
   MemorySanitizer reads every sample the decoder claimed to write, and the init plane does the
   same job here: a sample the decoder left unwritten and this loop then reads is a J1 report rather
   than a number nobody looked at. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <turbojpeg.h>

/* Upstream's count, one pass per format in the list below. */
#define NUMPF 5

int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size) {
    tjhandle handle = NULL;
    void *dstBuf = NULL;
    int width = 0, height = 0, precision, sampleSize, pfi;
    /* TJPF_RGB-TJPF_BGR share the same code paths, as do TJPF_RGBX-TJPF_XRGB and
       TJPF_RGBA-TJPF_ARGB.  Thus, the pixel formats below should be the minimum
       necessary to achieve full coverage. */
    enum TJPF pixelFormats[NUMPF] = {TJPF_RGB, TJPF_BGRX, TJPF_ABGR, TJPF_GRAY, TJPF_CMYK};

    if ((handle = tj3Init(TJINIT_DECOMPRESS)) == NULL) goto bailout;

    /* We ignore the return value of tj3DecompressHeader(), because malformed
       JPEG images that might expose issues in libjpeg-turbo might also have
       header errors that cause tj3DecompressHeader() to fail. */
    tj3DecompressHeader(handle, data, size);
    width = tj3Get(handle, TJPARAM_JPEGWIDTH);
    height = tj3Get(handle, TJPARAM_JPEGHEIGHT);
    precision = tj3Get(handle, TJPARAM_PRECISION);
    sampleSize = (precision > 8 ? 2 : 1);

    /* Ignore 0-pixel images and images larger than 1 Megapixel, as Google's
       OSS-Fuzz target for libjpeg-turbo did.  Casting width to (uint64_t)
       prevents integer overflow if width * height > INT_MAX. */
    if (width < 1 || height < 1 || (uint64_t)width * height > 1048576) goto bailout;

    for (pfi = 0; pfi < NUMPF; pfi++) {
        int w = width, h = height;
        int pf = pixelFormats[pfi], i;
        int64_t sum = 0;

        /* Test non-default decompression options on the first iteration. */
        tj3Set(handle, TJPARAM_BOTTOMUP, pfi == 0);
        tj3Set(handle, TJPARAM_FASTUPSAMPLE, pfi == 0);

        if (!tj3Get(handle, TJPARAM_LOSSLESS)) {
            tj3Set(handle, TJPARAM_FASTDCT, pfi == 0);

            /* Test IDCT scaling on the second and third iterations. */
            if (pfi == 1 || pfi == 2) {
                tjscalingfactor sf;
                sf.num = 1;
                sf.denom = pfi == 1 ? 2 : 8;
                tj3SetScalingFactor(handle, sf);
                w = TJSCALED(width, sf);
                h = TJSCALED(height, sf);
            } else
                tj3SetScalingFactor(handle, TJUNSCALED);

            /* Test partial image decompression on the second and fourth iterations,
               if the image is large enough. */
            if ((pfi == 1 || pfi == 3) && w >= 97 && h >= 75) {
                tjregion cr = {32, 16, 65, 59};
                tj3SetCroppingRegion(handle, cr);
            } else
                tj3SetCroppingRegion(handle, TJUNCROPPED);
            tj3Set(handle, TJPARAM_SCANLIMIT, 100);
        } else
            tj3Set(handle, TJPARAM_SCANLIMIT, 50);

        if ((dstBuf = tj3Alloc(w * h * tjPixelSize[pf] * sampleSize)) == NULL) goto bailout;

        if (precision == 8) {
            if (tj3Decompress8(handle, data, size, (unsigned char *)dstBuf, 0, pf) == 0) {
                /* Touch all of the output pixels in order to catch uninitialized reads
                   when using MemorySanitizer. */
                for (i = 0; i < w * h * tjPixelSize[pf]; i++) sum += ((unsigned char *)dstBuf)[i];
            } else if (!strcmp(tj3GetErrorStr(handle),
                               "Progressive JPEG image has more than 100 scans"))
                goto bailout;
        } else if (precision == 12) {
            if (tj3Decompress12(handle, data, size, (short *)dstBuf, 0, pf) == 0) {
                /* Touch all of the output pixels in order to catch uninitialized reads
                   when using MemorySanitizer. */
                for (i = 0; i < w * h * tjPixelSize[pf]; i++) sum += ((short *)dstBuf)[i];
            } else if (!strcmp(tj3GetErrorStr(handle),
                               "Progressive JPEG image has more than 100 scans"))
                goto bailout;
        } else {
            if (tj3Decompress16(handle, data, size, (unsigned short *)dstBuf, 0, pf) == 0) {
                /* Touch all of the output pixels in order to catch uninitialized reads
                   when using MemorySanitizer. */
                for (i = 0; i < w * h * tjPixelSize[pf]; i++)
                    sum += ((unsigned short *)dstBuf)[i];
            } else if (!strcmp(tj3GetErrorStr(handle),
                               "Progressive JPEG image has more than 100 scans"))
                goto bailout;
        }

        tj3Free(dstBuf);
        dstBuf = NULL;

        /* Prevent the sum above from being optimized out.  This test should never
           be true, but the compiler doesn't know that. */
        if (sum > ((1LL << precision) - 1LL) * 1048576LL * tjPixelSize[pf]) goto bailout;
    }

bailout:
    tj3Free(dstBuf);
    tj3Destroy(handle);
    return 0;
}
