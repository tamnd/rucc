/* Microsoft's bit scans, which cl.exe turns into one bsf or bsr where they are called.
 *
 * This compiler has no intrinsic by these names, so they are functions here and a call to one is a
 * call. They are reached from Microsoft's own headers rather than only from programs: the universal
 * CRT's <wchar.h> calls `_BitScanForward` in the inline `wmemchr` and `wmemcmp` it defines, and
 * <winnt.h> declares all four. So each one is declared in `<intrin.h>` the way Microsoft declares
 * it, as an ordinary function with external linkage, and the definition is in this archive, which a
 * link for an msvc row always has.
 *
 * Only on the msvc rows. mingw-w64 has its own copies, as inline functions in its headers, and a
 * second definition here would be one more name for a link to choose between.
 */

#if defined(__x86_64__) && defined(_WIN32) && defined(_MSC_VER)

/* Each one writes where the lowest or highest set bit is and answers 1, or answers 0 and leaves the
 * index alone when there is no set bit, which is what the instructions do with their destination
 * and what Microsoft documents. */

unsigned char _BitScanForward(unsigned long *index, unsigned long mask)
{
    if (mask == 0)
        return 0;
    *index = (unsigned long)__builtin_ctz(mask);
    return 1;
}

unsigned char _BitScanForward64(unsigned long *index, unsigned long long mask)
{
    if (mask == 0)
        return 0;
    *index = (unsigned long)__builtin_ctzll(mask);
    return 1;
}

unsigned char _BitScanReverse(unsigned long *index, unsigned long mask)
{
    if (mask == 0)
        return 0;
    *index = 31 - (unsigned long)__builtin_clz(mask);
    return 1;
}

unsigned char _BitScanReverse64(unsigned long *index, unsigned long long mask)
{
    if (mask == 0)
        return 0;
    *index = 63 - (unsigned long)__builtin_clzll(mask);
    return 1;
}

#endif
