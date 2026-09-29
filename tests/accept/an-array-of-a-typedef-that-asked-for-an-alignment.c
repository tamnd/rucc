/* accept: all */
/* An array is as aligned as its element, and when the element is a typedef that asked for an
   alignment that is the typedef's and not the one of the type behind it. This is the shape of
   mingw-w64's jmp_buf, where the struct is aligned to eight and the typedef of it to sixteen, and
   gcc gives all of these sixteen. tamnd/rucc#2151. The checks are array sizes rather than
   `_Static_assert` because this case runs under c89 as well. */

typedef __attribute__((aligned(16))) struct part { unsigned long long p[2]; } part_t;
typedef part_t alias_t;
typedef alias_t buf_t[16];
typedef part_t bufs_t[2][16];

struct holder {
    char tag;
    buf_t buf;
};

typedef int plain[__alignof__(struct part) == 8 ? 1 : -1];
typedef int named[__alignof__(part_t) == 16 && __alignof__(alias_t) == 16 ? 1 : -1];
typedef int arrays[__alignof__(buf_t) == 16 && __alignof__(bufs_t) == 16 ? 1 : -1];
typedef int sizes[sizeof(buf_t) == 256 && sizeof(bufs_t) == 512 ? 1 : -1];
typedef int member[__builtin_offsetof(struct holder, buf) == 16 ? 1 : -1];
/* A typedef of the array that asks for an alignment of its own is the answer for the array, and
   one that lowers its element's alignment lowers the array's with it. */
typedef int over[4] __attribute__((aligned(16)));
typedef int low __attribute__((aligned(2)));
typedef int owns[__alignof__(over) == 16 && __alignof__(low[4]) == 2 ? 1 : -1];
typedef int record[__alignof__(struct holder) == 16 && sizeof(struct holder) == 272 ? 1 : -1];

static char before;
static buf_t global;

int main(void) {
    buf_t local;
    return (int)((__SIZE_TYPE__)&global % 16 + (__SIZE_TYPE__)&local % 16) + before;
}
