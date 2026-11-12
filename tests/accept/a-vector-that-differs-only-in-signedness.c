/* accept: all */
/* Two vectors whose lanes are one integer type in its two signednesses, which GNU C lets an
   operator take as gcc does. NetBSD's NEON AES writes `t ^= carry & mask` with `t` an
   `int32x4_t` and the other two `uint32x4_t`. */

typedef int __attribute__((vector_size(16))) v4si;
typedef unsigned __attribute__((vector_size(16))) v4su;

v4si f(v4si t, v4su carry, v4su mask) {
  t ^= carry & mask;
  t += carry;
  v4su u = t ^ carry;
  return t & (v4si)u;
}
