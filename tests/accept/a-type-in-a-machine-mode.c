/* accept: all */
/* GNU's `mode`, which says the declared type is whatever type the machine has in that mode
   rather than the type that was written. The attribute is taken in every dialect, the same as
   gcc takes it, because a header that declares a fixed width integer this way is compiled under
   whatever the project asked for. The checks are array sizes rather than `_Static_assert`
   because this case runs under c89 as well. */

typedef unsigned int __attribute__((mode(QI))) u8;
typedef int __attribute__((mode(QI))) s8;
typedef unsigned int __attribute__((mode(HI))) u16;
typedef unsigned int __attribute__((mode(SI))) u32;
typedef unsigned int __attribute__((mode(DI))) u64;
typedef unsigned int __attribute__((mode(byte))) ubyte;
typedef unsigned int __attribute__((mode(word))) uword;
typedef unsigned int __attribute__((mode(pointer))) uptr;
typedef float __attribute__((mode(SF))) f32;
typedef float __attribute__((mode(DF))) f64;

typedef int widths[sizeof(u8) == 1 && sizeof(u16) == 2 && sizeof(u32) == 4 && sizeof(u64) == 8
                       ? 1
                       : -1];
typedef int named[sizeof(ubyte) == 1 && sizeof(uword) == sizeof(void *)
                          && sizeof(uptr) == sizeof(void *)
                      ? 1
                      : -1];
typedef int floats[sizeof(f32) == 4 && sizeof(f64) == 8 ? 1 : -1];

/* The signedness is the written type's and the mode says only the width, so the same mode on an
   `int` and on an `unsigned int` gives two types that disagree about what all ones means. */
typedef int signedness[(s8) - 1 < 0 && (u8) - 1 == 255 ? 1 : -1];

u32 f(u8 a, u16 b, u64 c) { return (u32) (a + b + c); }
