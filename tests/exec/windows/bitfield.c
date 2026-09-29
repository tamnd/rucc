/* Bit-field structs with ms_struct and gcc_struct, which choose the layout rule for one record.
 *
 * mingw-w64 lays bit-fields out the way MSVC does, and gcc_struct asks for the rule gcc uses on
 * Linux instead, one record at a time. ms_struct asks for the Windows rule, which is what the
 * target gives anyway. Every line is compared with gcc's. Document 11.4. */
#include <stddef.h>
#include <stdio.h>
#include <string.h>

struct __attribute__((gcc_struct)) ga { unsigned x : 3; char y; };
struct __attribute__((ms_struct)) ma { unsigned x : 3; char y; };
struct gb { char x : 3; int y : 5; } __attribute__((gcc_struct));
struct mb { char x : 3; int y : 5; } __attribute__((ms_struct));
struct __attribute__((__gcc_struct__)) gc { char x; int : 20; };
struct __attribute__((gcc_struct)) gd { char x : 1; int : 0; char y : 1; };
struct __attribute__((gcc_struct)) ge { char x; long long y : 40; int z : 20; };
struct __attribute__((gcc_struct, packed)) gf { char x; int y : 30; };
union __attribute__((gcc_struct)) gu { unsigned x : 3; char y; };
typedef struct { unsigned char x : 4; unsigned short y : 4; unsigned z : 4; } __attribute__((gcc_struct)) gt;
typedef struct { unsigned char x : 4; unsigned short y : 4; unsigned z : 4; } __attribute__((ms_struct)) mt;

#define SHOW(t) printf("%s size %u align %u\n", #t, (unsigned)sizeof(t), (unsigned)_Alignof(t))

static void bytes(const char *name, const void *p, size_t n) {
    const unsigned char *b = p;
    printf("%s bytes", name);
    for (size_t i = 0; i < n; i++)
        printf(" %02x", b[i]);
    printf("\n");
}

int main(void) {
    SHOW(struct ga);
    SHOW(struct ma);
    SHOW(struct gb);
    SHOW(struct mb);
    SHOW(struct gc);
    SHOW(struct gd);
    SHOW(struct ge);
    SHOW(struct gf);
    SHOW(union gu);
    SHOW(gt);
    SHOW(mt);
    printf("ga.y at %u\n", (unsigned)offsetof(struct ga, y));
    printf("ma.y at %u\n", (unsigned)offsetof(struct ma, y));

    struct gb gb;
    memset(&gb, 0, sizeof gb);
    gb.x = -3;
    gb.y = 11;
    printf("gb %d %d\n", gb.x, gb.y);
    bytes("gb", &gb, sizeof gb);

    struct ge ge;
    memset(&ge, 0, sizeof ge);
    ge.x = 'q';
    ge.y = 0x123456789aLL;
    ge.z = -5;
    printf("ge %c %llx %d\n", ge.x, (unsigned long long)ge.y, ge.z);
    bytes("ge", &ge, sizeof ge);

    gt t;
    memset(&t, 0, sizeof t);
    t.x = 9;
    t.y = 10;
    t.z = 11;
    printf("gt %u %u %u\n", t.x, t.y, t.z);
    bytes("gt", &t, sizeof t);
    return 0;
}
