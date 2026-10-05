/* A workload against an instrumented libjpeg-turbo, with answers it has to get right. */
/* The seventh library run under the monitor, and the first whose storage is a set of planes cut
   into eight by eight blocks rather than a rectangle of pixels or a run of bytes. libjpeg-turbo
   keeps every buffer it decodes into as an array of row pointers that its memory manager carves
   out of pools it frees all at once, it builds the same code three times over for eight, twelve
   and sixteen bit samples by including one file under three settings of a macro, and its errors
   leave through longjmp from wherever in the decoder they were noticed. None of the other six does
   any of that.

   The answers are arithmetic on an image this program draws for itself, so they do not depend on
   which version of the library this is built against. Lossless JPEG has to give back exactly what
   it was given, at every precision and with every predictor. Lossy JPEG has to come back close,
   and close is a mean error this program measures against a bound with plenty of room in it,
   because the exact numbers belong to the version and the bound does not. A lossless transform
   applied enough times to come back where it started has to decode to exactly what the original
   decodes to, because nothing about the coefficients moved. The compressed sizes are not checked,
   for the reason the zlib workload gives.

   turbojpeg.h is included rather than written out, because tjtransform and tjregion are passed
   by value and their layout has to be the one the library was compiled with. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <turbojpeg.h>

/* A multiple of sixteen both ways, so every subsampling has whole MCUs and every transform below is
   a perfect one, and small enough that the whole run takes well under a second at every level. */
#define WIDTH 96
#define HEIGHT 80

static unsigned char rgb[WIDTH * HEIGHT * 3];
static unsigned char grey[WIDTH * HEIGHT];
static short rgb12[WIDTH * HEIGHT * 3];
static unsigned short rgb16[WIDTH * HEIGHT * 3];

static int failures;

static void fail(const char *what, long detail) {
    printf("wrong: %s (%ld)\n", what, detail);
    failures++;
}

/* Smooth gradients with a little noise on top. The gradients are what a lossy coder is good at, so
   its error stays small, and the noise is what gives a lossless predictor something to get wrong
   if its arithmetic is off by one anywhere. */
static void draw(void) {
    unsigned long state = 2463534242UL;
    int x, y, c;
    for (y = 0; y < HEIGHT; y++) {
        for (x = 0; x < WIDTH; x++) {
            int at = (y * WIDTH + x) * 3;
            int base[3];
            base[0] = x * 255 / (WIDTH - 1);
            base[1] = y * 255 / (HEIGHT - 1);
            base[2] = (x + y) * 255 / (WIDTH + HEIGHT - 2);
            for (c = 0; c < 3; c++) {
                int value;
                state = state * 1103515245UL + 12345UL;
                value = base[c] + (int)((state >> 16) % 7) - 3;
                if (value < 0) value = 0;
                if (value > 255) value = 255;
                rgb[at + c] = (unsigned char)value;
                rgb12[at + c] = (short)(value * 16 + (int)((state >> 8) % 16));
                rgb16[at + c] = (unsigned short)(value * 257 + (int)((state >> 4) % 200));
            }
            grey[y * WIDTH + x] = (unsigned char)((rgb[at] * 77 + rgb[at + 1] * 150 +
                                                   rgb[at + 2] * 29) >> 8);
        }
    }
}

/* Mean absolute difference over n samples, times a hundred so it stays an integer. */
static long error8(const unsigned char *a, const unsigned char *b, long n) {
    long total = 0, i;
    for (i = 0; i < n; i++) total += a[i] > b[i] ? a[i] - b[i] : b[i] - a[i];
    return total * 100 / n;
}

static long error12(const short *a, const short *b, long n) {
    long total = 0, i;
    for (i = 0; i < n; i++) total += a[i] > b[i] ? a[i] - b[i] : b[i] - a[i];
    return total * 100 / n;
}

/* Compresses the eight bit image and returns the JPEG, which the caller frees. */
static unsigned char *pack8(tjhandle tj, const unsigned char *src, int format, size_t *size) {
    unsigned char *jpeg = NULL;
    *size = 0;
    if (tj3Compress8(tj, src, WIDTH, 0, HEIGHT, format, &jpeg, size) != 0) {
        tj3Free(jpeg);
        return NULL;
    }
    return jpeg;
}

/* Every subsampling, under each of the four ways of writing the entropy coded data, decoded back
   and held to a mean error. Gray is coded from the grey image, since a grey JPEG of a colour
   picture is a different picture. */
static void lossy(tjhandle in, tjhandle out) {
    static unsigned char back[WIDTH * HEIGHT * 3];
    static const int samps[] = {TJSAMP_444, TJSAMP_422, TJSAMP_420, TJSAMP_440, TJSAMP_411,
                                TJSAMP_GRAY};
    int s, way;
    for (s = 0; s < 6; s++) {
        for (way = 0; way < 4; way++) {
            int gray = samps[s] == TJSAMP_GRAY;
            int format = gray ? TJPF_GRAY : TJPF_RGB;
            const unsigned char *src = gray ? grey : rgb;
            long n = (long)WIDTH * HEIGHT * (gray ? 1 : 3);
            unsigned char *jpeg;
            size_t size;
            long err;
            tj3Set(in, TJPARAM_QUALITY, 92);
            tj3Set(in, TJPARAM_SUBSAMP, samps[s]);
            tj3Set(in, TJPARAM_PROGRESSIVE, way == 1);
            tj3Set(in, TJPARAM_ARITHMETIC, way == 2);
            tj3Set(in, TJPARAM_OPTIMIZE, way == 3);
            tj3Set(in, TJPARAM_RESTARTBLOCKS, way == 3 ? 3 : 0);
            jpeg = pack8(in, src, format, &size);
            if (jpeg == NULL) {
                fail("a lossy compression failed", s * 10 + way);
                continue;
            }
            if (tj3DecompressHeader(out, jpeg, size) != 0 ||
                tj3Get(out, TJPARAM_JPEGWIDTH) != WIDTH ||
                tj3Get(out, TJPARAM_JPEGHEIGHT) != HEIGHT ||
                tj3Get(out, TJPARAM_SUBSAMP) != samps[s] ||
                tj3Get(out, TJPARAM_PROGRESSIVE) != (way == 1) ||
                tj3Get(out, TJPARAM_ARITHMETIC) != (way == 2)) {
                fail("a lossy header came back different", s * 10 + way);
            } else if (tj3Decompress8(out, jpeg, size, back, 0, format) != 0) {
                fail("a lossy decompression failed", s * 10 + way);
            } else if ((err = error8(src, back, n)) > 400) {
                fail("a lossy round trip is too far off, mean error times 100", err);
            }
            tj3Free(jpeg);
        }
    }
    tj3Set(in, TJPARAM_PROGRESSIVE, 0);
    tj3Set(in, TJPARAM_ARITHMETIC, 0);
    tj3Set(in, TJPARAM_OPTIMIZE, 0);
    tj3Set(in, TJPARAM_RESTARTBLOCKS, 0);
}

/* Twelve bit lossy, which is the second copy of the DCT and colour code the library builds. */
static void lossy12(tjhandle in, tjhandle out) {
    static short back[WIDTH * HEIGHT * 3];
    unsigned char *jpeg = NULL;
    size_t size = 0;
    long err;
    tj3Set(in, TJPARAM_PRECISION, 12);
    tj3Set(in, TJPARAM_QUALITY, 95);
    tj3Set(in, TJPARAM_SUBSAMP, TJSAMP_420);
    if (tj3Compress12(in, rgb12, WIDTH, 0, HEIGHT, TJPF_RGB, &jpeg, &size) != 0) {
        fail("twelve bit lossy compression failed", 0);
    } else if (tj3DecompressHeader(out, jpeg, size) != 0 ||
               tj3Get(out, TJPARAM_PRECISION) != 12) {
        fail("twelve bit lossy header came back different", 0);
    } else if (tj3Decompress12(out, jpeg, size, back, 0, TJPF_RGB) != 0) {
        fail("twelve bit lossy decompression failed", 0);
    } else if ((err = error12(rgb12, back, (long)WIDTH * HEIGHT * 3)) > 6400) {
        fail("twelve bit lossy round trip is too far off, mean error times 100", err);
    }
    tj3Free(jpeg);
    tj3Set(in, TJPARAM_PRECISION, 8);
}

/* Lossless at all three precisions, with all seven predictors, and once with a point transform,
   which drops low bits on the way in and has to give back exactly the high ones. */
static void lossless(tjhandle in, tjhandle out) {
    static unsigned char back8[WIDTH * HEIGHT * 3];
    static short back12[WIDTH * HEIGHT * 3];
    static unsigned short back16[WIDTH * HEIGHT * 3];
    long n = (long)WIDTH * HEIGHT * 3, i;
    int psv, bits;
    tj3Set(in, TJPARAM_LOSSLESS, 1);
    for (bits = 8; bits <= 16; bits += 4) {
        for (psv = 1; psv <= 8; psv++) {
            int pt = psv == 8 ? 2 : 0;
            unsigned char *jpeg = NULL;
            size_t size = 0;
            int got;
            tj3Set(in, TJPARAM_PRECISION, bits);
            tj3Set(in, TJPARAM_LOSSLESSPSV, psv == 8 ? 1 : psv);
            tj3Set(in, TJPARAM_LOSSLESSPT, pt);
            if (bits == 8)
                got = tj3Compress8(in, rgb, WIDTH, 0, HEIGHT, TJPF_RGB, &jpeg, &size);
            else if (bits == 12)
                got = tj3Compress12(in, rgb12, WIDTH, 0, HEIGHT, TJPF_RGB, &jpeg, &size);
            else
                got = tj3Compress16(in, rgb16, WIDTH, 0, HEIGHT, TJPF_RGB, &jpeg, &size);
            if (got != 0) {
                fail("a lossless compression failed", bits * 10 + psv);
                tj3Free(jpeg);
                continue;
            }
            if (tj3DecompressHeader(out, jpeg, size) != 0 || !tj3Get(out, TJPARAM_LOSSLESS) ||
                tj3Get(out, TJPARAM_PRECISION) != bits) {
                fail("a lossless header came back different", bits * 10 + psv);
                tj3Free(jpeg);
                continue;
            }
            if (bits == 8)
                got = tj3Decompress8(out, jpeg, size, back8, 0, TJPF_RGB);
            else if (bits == 12)
                got = tj3Decompress12(out, jpeg, size, back12, 0, TJPF_RGB);
            else
                got = tj3Decompress16(out, jpeg, size, back16, 0, TJPF_RGB);
            tj3Free(jpeg);
            if (got != 0) {
                fail("a lossless decompression failed", bits * 10 + psv);
                continue;
            }
            for (i = 0; i < n; i++) {
                long want, have;
                if (bits == 8) {
                    want = rgb[i];
                    have = back8[i];
                } else if (bits == 12) {
                    want = rgb12[i];
                    have = back12[i];
                } else {
                    want = rgb16[i];
                    have = back16[i];
                }
                if (have != (want >> pt) << pt) {
                    fail("a lossless round trip changed a sample", bits * 1000 + psv * 100);
                    break;
                }
            }
        }
    }
    tj3Set(in, TJPARAM_LOSSLESS, 0);
    tj3Set(in, TJPARAM_LOSSLESSPT, 0);
    tj3Set(in, TJPARAM_PRECISION, 8);
}

/* Scaled decompression, which takes the reduced size inverse DCTs, and a cropped one, which skips
   whole blocks and has to give exactly the pixels a full decode gives for that region. Scaling is
   held to the mean of the picture rather than to its pixels, because a scaled IDCT keeps the DC
   term and nothing else about it is an answer. */
static void partial(tjhandle in, tjhandle out) {
    static unsigned char full[WIDTH * HEIGHT * 3];
    static unsigned char part[WIDTH * HEIGHT * 3];
    static const int dens[] = {2, 4, 8};
    unsigned char *jpeg;
    size_t size;
    int d, x, y;
    long want = 0, have;
    tjregion crop = {16, 16, 48, 32};
    tj3Set(in, TJPARAM_QUALITY, 90);
    tj3Set(in, TJPARAM_SUBSAMP, TJSAMP_444);
    jpeg = pack8(in, rgb, TJPF_RGB, &size);
    if (jpeg == NULL) {
        fail("compression for the partial decodes failed", 0);
        return;
    }
    tj3DecompressHeader(out, jpeg, size);
    if (tj3Decompress8(out, jpeg, size, full, 0, TJPF_RGB) != 0) {
        fail("the full decode failed", 0);
        tj3Free(jpeg);
        return;
    }
    for (x = 0; x < WIDTH * HEIGHT * 3; x++) want += full[x];
    want /= WIDTH * HEIGHT * 3;
    for (d = 0; d < 3; d++) {
        tjscalingfactor sf;
        int w, h;
        sf.num = 1;
        sf.denom = dens[d];
        w = TJSCALED(WIDTH, sf);
        h = TJSCALED(HEIGHT, sf);
        tj3DecompressHeader(out, jpeg, size);
        if (tj3SetScalingFactor(out, sf) != 0 ||
            tj3Decompress8(out, jpeg, size, part, 0, TJPF_RGB) != 0) {
            fail("a scaled decode failed", dens[d]);
            continue;
        }
        have = 0;
        for (x = 0; x < w * h * 3; x++) have += part[x];
        have /= w * h * 3;
        if (have - want > 3 || want - have > 3) fail("a scaled decode has the wrong mean", have);
    }
    tj3SetScalingFactor(out, TJUNSCALED);
    tj3DecompressHeader(out, jpeg, size);
    if (tj3SetCroppingRegion(out, crop) != 0 ||
        tj3Decompress8(out, jpeg, size, part, 0, TJPF_RGB) != 0) {
        fail("the cropped decode failed", 0);
    } else {
        for (y = 0; y < crop.h; y++) {
            for (x = 0; x < crop.w * 3; x++) {
                unsigned char a = part[y * crop.w * 3 + x];
                unsigned char b = full[((y + crop.y) * WIDTH + crop.x) * 3 + x];
                if (a != b) {
                    fail("the cropped decode differs from the full one", y * 1000 + x);
                    y = crop.h;
                    break;
                }
            }
        }
    }
    tj3SetCroppingRegion(out, TJUNCROPPED);
    tj3Free(jpeg);
}

/* Applies one transform to a JPEG and returns the result in place of it. */
static unsigned char *turn(tjhandle tx, unsigned char *jpeg, size_t *size, int op) {
    unsigned char *next = NULL;
    size_t got = 0;
    tjtransform how;
    memset(&how, 0, sizeof how);
    how.op = op;
    how.options = TJXOPT_PERFECT;
    if (tj3Transform(tx, jpeg, *size, 1, &next, &got, &how) != 0) {
        tj3Free(next);
        tj3Free(jpeg);
        return NULL;
    }
    tj3Free(jpeg);
    *size = got;
    return next;
}

/* Rotating four times, flipping twice and transposing twice each come back to where they started,
   so each has to decode to exactly what the untouched JPEG decodes to. A single quarter turn has to
   swap the two dimensions. */
static void transforms(tjhandle in, tjhandle out, tjhandle tx) {
    static unsigned char first[WIDTH * HEIGHT * 3];
    static unsigned char again[WIDTH * HEIGHT * 3];
    static const int ops[] = {TJXOP_ROT90, TJXOP_HFLIP, TJXOP_VFLIP, TJXOP_TRANSPOSE,
                              TJXOP_ROT180};
    static const int times[] = {4, 2, 2, 2, 2};
    unsigned char *base;
    size_t base_size;
    int o, k;
    tj3Set(in, TJPARAM_QUALITY, 85);
    tj3Set(in, TJPARAM_SUBSAMP, TJSAMP_420);
    base = pack8(in, rgb, TJPF_RGB, &base_size);
    if (base == NULL) {
        fail("compression for the transforms failed", 0);
        return;
    }
    tj3DecompressHeader(out, base, base_size);
    if (tj3Decompress8(out, base, base_size, first, 0, TJPF_RGB) != 0) {
        fail("decoding the untransformed image failed", 0);
        tj3Free(base);
        return;
    }
    for (o = 0; o < 5; o++) {
        unsigned char *jpeg = tj3Alloc(base_size);
        size_t size = base_size;
        if (jpeg == NULL) {
            fail("tj3Alloc failed", o);
            continue;
        }
        memcpy(jpeg, base, base_size);
        for (k = 0; k < times[o] && jpeg != NULL; k++) {
            jpeg = turn(tx, jpeg, &size, ops[o]);
            if (jpeg != NULL && k == 0 && ops[o] == TJXOP_ROT90) {
                tj3DecompressHeader(out, jpeg, size);
                if (tj3Get(out, TJPARAM_JPEGWIDTH) != HEIGHT ||
                    tj3Get(out, TJPARAM_JPEGHEIGHT) != WIDTH)
                    fail("a quarter turn did not swap the dimensions", 0);
            }
        }
        if (jpeg == NULL) {
            fail("a transform failed", ops[o]);
            continue;
        }
        tj3DecompressHeader(out, jpeg, size);
        if (tj3Decompress8(out, jpeg, size, again, 0, TJPF_RGB) != 0)
            fail("decoding a transformed image failed", ops[o]);
        else if (memcmp(first, again, sizeof first) != 0)
            fail("a transform that comes back round decoded differently", ops[o]);
        tj3Free(jpeg);
    }
    tj3Free(base);
}

/* The planar YUV path, encoded from RGB and decoded back without any JPEG in between, so the only
   loss is the colour conversion and the subsampling. */
static void yuv(tjhandle in, tjhandle out) {
    static unsigned char back[WIDTH * HEIGHT * 3];
    unsigned char *planes;
    size_t need = tj3YUVBufSize(WIDTH, 4, HEIGHT, TJSAMP_444);
    long err;
    planes = tj3Alloc(need);
    if (planes == NULL) {
        fail("tj3Alloc for the planes failed", (long)need);
        return;
    }
    tj3Set(in, TJPARAM_SUBSAMP, TJSAMP_444);
    tj3Set(out, TJPARAM_SUBSAMP, TJSAMP_444);
    if (tj3EncodeYUV8(in, rgb, WIDTH, 0, HEIGHT, TJPF_RGB, planes, 4) != 0)
        fail("encoding to YUV failed", 0);
    else if (tj3DecodeYUV8(out, planes, 4, back, WIDTH, 0, HEIGHT, TJPF_RGB) != 0)
        fail("decoding from YUV failed", 0);
    else if ((err = error8(rgb, back, (long)WIDTH * HEIGHT * 3)) > 150)
        fail("the YUV round trip is too far off, mean error times 100", err);
    tj3Free(planes);
}

/* A JPEG cut off halfway and one with its scan data overwritten. The library reports both through
   its error manager, which leaves by longjmp from inside the decoder, and the same handle has to
   decode a good image correctly afterwards. Truncation is a warning rather than an error by
   default, so it is made an error here to take the longjmp path every time. */
static void broken(tjhandle in, tjhandle out) {
    static unsigned char back[WIDTH * HEIGHT * 3];
    static unsigned char good[WIDTH * HEIGHT * 3];
    unsigned char *jpeg;
    unsigned char *bad;
    size_t size, i;
    tj3Set(in, TJPARAM_QUALITY, 80);
    tj3Set(in, TJPARAM_SUBSAMP, TJSAMP_422);
    jpeg = pack8(in, rgb, TJPF_RGB, &size);
    if (jpeg == NULL) {
        fail("compression for the broken inputs failed", 0);
        return;
    }
    tj3DecompressHeader(out, jpeg, size);
    if (tj3Decompress8(out, jpeg, size, good, 0, TJPF_RGB) != 0) {
        fail("decoding the good image failed", 0);
        tj3Free(jpeg);
        return;
    }
    tj3Set(out, TJPARAM_STOPONWARNING, 1);
    if (tj3Decompress8(out, jpeg, size / 2, back, 0, TJPF_RGB) == 0)
        fail("a truncated JPEG decoded without complaint", 0);
    bad = malloc(size);
    if (bad != NULL) {
        memcpy(bad, jpeg, size);
        for (i = size / 3; i < size - 2; i += 7) bad[i] = 0xff;
        tj3Decompress8(out, bad, size, back, 0, TJPF_RGB);
        free(bad);
    }
    tj3Set(out, TJPARAM_STOPONWARNING, 0);
    tj3DecompressHeader(out, jpeg, size);
    if (tj3Decompress8(out, jpeg, size, back, 0, TJPF_RGB) != 0)
        fail("the handle did not recover after a broken input", 0);
    else if (memcmp(good, back, sizeof back) != 0)
        fail("the handle decoded differently after a broken input", 0);
    tj3Free(jpeg);
}

int main(void) {
    tjhandle in = tj3Init(TJINIT_COMPRESS);
    tjhandle out = tj3Init(TJINIT_DECOMPRESS);
    tjhandle tx = tj3Init(TJINIT_TRANSFORM);
    if (in == NULL || out == NULL || tx == NULL) {
        printf("wrong: a handle could not be made\n");
        return 1;
    }
    draw();
    lossy(in, out);
    lossy12(in, out);
    lossless(in, out);
    partial(in, out);
    transforms(in, out, tx);
    yuv(in, out);
    broken(in, out);
    tj3Destroy(in);
    tj3Destroy(out);
    tj3Destroy(tx);
    if (failures != 0) {
        printf("%d answers wrong\n", failures);
        return 1;
    }
    printf("all answers correct\n");
    return 0;
}
