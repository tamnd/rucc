/* Sizes and offsets of bit-field structs, which mingw-w64 lays out the way MSVC does.
 *
 * gcc for mingw turns -mms-bitfields on by default, so a bit-field starts a new unit whenever its
 * declared type changes size, and a zero width one aligns the next member only after a bit-field.
 * Every line is compared with gcc's. Document 05.2. */
#include <stddef.h>
#include <stdio.h>

struct a { unsigned x : 3; char y; };
struct b { char x : 3; int y : 5; };
struct c { int x : 3; char y : 2; char z : 2; };
struct d { char x; int : 0; char y; };
struct e { char x : 1; int : 0; char y : 1; };
struct f { long long x : 3; char y; };
struct g { short x : 9; short y : 9; };
struct h { char x; long long y : 40; int z : 20; };
#pragma pack(push, 1)
struct i { char x; int y : 17; short z; };
#pragma pack(pop)
struct j { unsigned char x : 4; unsigned short y : 4; unsigned int z : 4; };

#define SHOW(t) printf("%s size %u align %u\n", #t, (unsigned)sizeof(struct t), (unsigned)_Alignof(struct t))

int main(void) {
    SHOW(a);
    SHOW(b);
    SHOW(c);
    SHOW(d);
    SHOW(e);
    SHOW(f);
    SHOW(g);
    SHOW(h);
    SHOW(i);
    SHOW(j);
    printf("a.y at %u\n", (unsigned)offsetof(struct a, y));
    printf("d.y at %u\n", (unsigned)offsetof(struct d, y));
    printf("f.y at %u\n", (unsigned)offsetof(struct f, y));
    printf("i.z at %u\n", (unsigned)offsetof(struct i, z));
    struct g g = {0};
    g.x = -200;
    g.y = 255;
    printf("g %d %d\n", g.x, g.y);
    return 0;
}
