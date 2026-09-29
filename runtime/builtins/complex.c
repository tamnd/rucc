/* Multiplying and dividing complex numbers, which C says has to get infinities right and which the
 * obvious four line formula does not.
 *
 * Design: spec/12-abi-and-runtime.md section 12.8 and spec/cross-compile/10-runtime.md section 10.2.
 * The lowering in crates/rucc-lower/src/body.rs turns `z * w` and `z / w` on complex operands into a
 * call, and complex_routine there picks the name: libgcc's, spelled after the machine mode of one
 * half, so `__muldc3` for double, `__mulsc3` for float, `__mulxc3` for the x87 long double and
 * `__multc3` for the quadruple one. Until this file there was nothing on the other end of that call
 * on a target whose link line has no libgcc, which is all of ours, and on x86_64-windows-gnu the gcc
 * torture tests that divide two complex values stopped at the link with `__divdc3` undefined.
 *
 * # Why a call at all
 *
 * The product of a+bi and c+di is (ac-bd)+(ad+bc)i and that is four multiplications and two adds,
 * which the lowering could write in place. It does not because of Annex G. An infinite complex value
 * is one with either half infinite, whatever the other half is, and the formula gets that wrong: (inf
 * + 0i) times (0 + 1i) has ac-bd = inf*0 - 0*1, which is a NaN, and the answer C wants is 0 + inf i.
 * So the formula runs first and when both halves come out NaN the routine looks at what went in,
 * boxes each infinite operand down to a unit vector pointing the same way, turns the NaNs that the
 * boxing left behind into zeros, and runs the formula again scaled back up to infinity. That is the
 * whole of Annex G.5.1 paragraph 6 and it is copied here from the recovery code libgcc has, because
 * a program that was tested against GCC should get the same halves back.
 *
 * # Division
 *
 * Division has the same recovery at the end and a harder problem at the start, which is that the
 * textbook denominator c*c + d*d overflows long before the quotient does. The routine below is the
 * one libgcc has used since GCC 12, by Patrick McGehearty: Smith's method, which divides through by
 * the larger of c and d so that the denominator is at most twice that one, plus a scaling step that
 * moves operands close to the bottom or the top of the range into the middle first. The gcc torture
 * tests cdivchk-1 to cdivchk-3 check exactly those edges, which is why this is not plain Smith.
 *
 * The scaling multiplies and divides by constants rather than calling scalbn, and every other thing
 * it asks of a value is a comparison or a sign bit. So nothing here needs libm, which a freestanding
 * link does not have, and the builtins below all turn into instructions or into the soft float calls
 * next door in double.c and quad.c on a target with no floating point unit.
 *
 * Float divides in double, the way libgcc does on every target with double in hardware: the squares
 * of two floats cannot overflow a double, so the textbook formula is exact enough there and needs no
 * scaling. Multiplication stays in the format it was asked in because it has no denominator to go
 * wrong.
 *
 * # The long double copy
 *
 * The x87 and quadruple routines are the double ones with the types changed, and they are written out
 * rather than stamped from a macro for the reason div.c gives about its two widths. Which of the two
 * names a target gets is __LDBL_MANT_DIG__: 64 is the x87 format and 113 the quadruple one. A target
 * where long double is double gets neither, because the lowering names the double routine for it.
 *
 * # The value that comes back
 *
 * Built through a union with an array of two, which is the layout C gives a complex type in 6.2.5,
 * rather than by writing to __real__ and __imag__, so that this file asks nothing of the compiler
 * that the rest of the runtime does not already ask.
 */

#define INFINITY_F __builtin_inff()
#define INFINITY_D __builtin_inf()
#define INFINITY_L __builtin_infl()

/* One sign bit on a value that is otherwise one or zero, which is how an infinity is boxed. */
#define BOX_F(v) __builtin_copysignf(__builtin_isinf(v) ? 1.0f : 0.0f, v)
#define BOX_D(v) __builtin_copysign(__builtin_isinf(v) ? 1.0 : 0.0, v)
#define BOX_L(v) __builtin_copysignl(__builtin_isinf(v) ? 1.0L : 0.0L, v)

typedef union {
    float _Complex z;
    float part[2];
} complex_float;

typedef union {
    double _Complex z;
    double part[2];
} complex_double;

typedef union {
    long double _Complex z;
    long double part[2];
} complex_long_double;

float _Complex __mulsc3(float a, float b, float c, float d) {
    float ac = a * c, bd = b * d, ad = a * d, bc = b * c;
    float x = ac - bd, y = ad + bc;
    if (__builtin_isnan(x) && __builtin_isnan(y)) {
        int again = 0;
        if (__builtin_isinf(a) || __builtin_isinf(b)) {
            a = BOX_F(a);
            b = BOX_F(b);
            if (__builtin_isnan(c)) c = __builtin_copysignf(0.0f, c);
            if (__builtin_isnan(d)) d = __builtin_copysignf(0.0f, d);
            again = 1;
        }
        if (__builtin_isinf(c) || __builtin_isinf(d)) {
            c = BOX_F(c);
            d = BOX_F(d);
            if (__builtin_isnan(a)) a = __builtin_copysignf(0.0f, a);
            if (__builtin_isnan(b)) b = __builtin_copysignf(0.0f, b);
            again = 1;
        }
        if (!again && (__builtin_isinf(ac) || __builtin_isinf(bd) || __builtin_isinf(ad) ||
                       __builtin_isinf(bc))) {
            if (__builtin_isnan(a)) a = __builtin_copysignf(0.0f, a);
            if (__builtin_isnan(b)) b = __builtin_copysignf(0.0f, b);
            if (__builtin_isnan(c)) c = __builtin_copysignf(0.0f, c);
            if (__builtin_isnan(d)) d = __builtin_copysignf(0.0f, d);
            again = 1;
        }
        if (again) {
            x = INFINITY_F * (a * c - b * d);
            y = INFINITY_F * (a * d + b * c);
        }
    }
    complex_float r;
    r.part[0] = x;
    r.part[1] = y;
    return r.z;
}

float _Complex __divsc3(float a, float b, float c, float d) {
    double denom = (double)c * c + (double)d * d;
    float x = (float)(((double)a * c + (double)b * d) / denom);
    float y = (float)(((double)b * c - (double)a * d) / denom);
    if (__builtin_isnan(x) && __builtin_isnan(y)) {
        if (c == 0.0f && d == 0.0f && (!__builtin_isnan(a) || !__builtin_isnan(b))) {
            x = __builtin_copysignf(INFINITY_F, c) * a;
            y = __builtin_copysignf(INFINITY_F, c) * b;
        } else if ((__builtin_isinf(a) || __builtin_isinf(b)) && __builtin_isfinite(c) &&
                   __builtin_isfinite(d)) {
            a = BOX_F(a);
            b = BOX_F(b);
            x = INFINITY_F * (a * c + b * d);
            y = INFINITY_F * (b * c - a * d);
        } else if ((__builtin_isinf(c) || __builtin_isinf(d)) && __builtin_isfinite(a) &&
                   __builtin_isfinite(b)) {
            c = BOX_F(c);
            d = BOX_F(d);
            x = 0.0f * (a * c + b * d);
            y = 0.0f * (b * c - a * d);
        }
    }
    complex_float r;
    r.part[0] = x;
    r.part[1] = y;
    return r.z;
}

double _Complex __muldc3(double a, double b, double c, double d) {
    double ac = a * c, bd = b * d, ad = a * d, bc = b * c;
    double x = ac - bd, y = ad + bc;
    if (__builtin_isnan(x) && __builtin_isnan(y)) {
        int again = 0;
        if (__builtin_isinf(a) || __builtin_isinf(b)) {
            a = BOX_D(a);
            b = BOX_D(b);
            if (__builtin_isnan(c)) c = __builtin_copysign(0.0, c);
            if (__builtin_isnan(d)) d = __builtin_copysign(0.0, d);
            again = 1;
        }
        if (__builtin_isinf(c) || __builtin_isinf(d)) {
            c = BOX_D(c);
            d = BOX_D(d);
            if (__builtin_isnan(a)) a = __builtin_copysign(0.0, a);
            if (__builtin_isnan(b)) b = __builtin_copysign(0.0, b);
            again = 1;
        }
        if (!again && (__builtin_isinf(ac) || __builtin_isinf(bd) || __builtin_isinf(ad) ||
                       __builtin_isinf(bc))) {
            if (__builtin_isnan(a)) a = __builtin_copysign(0.0, a);
            if (__builtin_isnan(b)) b = __builtin_copysign(0.0, b);
            if (__builtin_isnan(c)) c = __builtin_copysign(0.0, c);
            if (__builtin_isnan(d)) d = __builtin_copysign(0.0, d);
            again = 1;
        }
        if (again) {
            x = INFINITY_D * (a * c - b * d);
            y = INFINITY_D * (a * d + b * c);
        }
    }
    complex_double r;
    r.part[0] = x;
    r.part[1] = y;
    return r.z;
}

/* The four limits the scaling in the division compares against, named the way libgcc names them.
 * RBIG is half the largest value, above which a halving keeps the denominator finite. RMIN is the
 * smallest normal value, RMIN2 the machine epsilon, RMINSCAL its reciprocal and the factor the small
 * operands are scaled up by, and RMAX2 the largest value that scaling cannot push past the top. */
#define RBIG_D (__DBL_MAX__ / 2.0)
#define RMIN_D __DBL_MIN__
#define RMIN2_D __DBL_EPSILON__
#define RMINSCAL_D (1.0 / __DBL_EPSILON__)
#define RMAX2_D (RBIG_D * RMIN2_D)

double _Complex __divdc3(double a, double b, double c, double d) {
    double ratio, denom, x, y;
    if (__builtin_fabs(c) < __builtin_fabs(d)) {
        if (__builtin_fabs(d) >= RBIG_D) {
            a /= 2;
            b /= 2;
            c /= 2;
            d /= 2;
        }
        if (__builtin_fabs(d) < RMIN2_D ||
            (((__builtin_fabs(a) < RMIN_D && __builtin_fabs(b) < RMAX2_D) ||
              (__builtin_fabs(b) < RMIN_D && __builtin_fabs(a) < RMAX2_D)) &&
             __builtin_fabs(d) < RMAX2_D)) {
            a *= RMINSCAL_D;
            b *= RMINSCAL_D;
            c *= RMINSCAL_D;
            d *= RMINSCAL_D;
        }
        ratio = c / d;
        denom = c * ratio + d;
        if (__builtin_fabs(ratio) > RMIN_D) {
            x = (a * ratio + b) / denom;
            y = (b * ratio - a) / denom;
        } else {
            x = (c * (a / d) + b) / denom;
            y = (c * (b / d) - a) / denom;
        }
    } else {
        if (__builtin_fabs(c) >= RBIG_D) {
            a /= 2;
            b /= 2;
            c /= 2;
            d /= 2;
        }
        if (__builtin_fabs(c) < RMIN2_D ||
            (((__builtin_fabs(a) < RMIN_D && __builtin_fabs(b) < RMAX2_D) ||
              (__builtin_fabs(b) < RMIN_D && __builtin_fabs(a) < RMAX2_D)) &&
             __builtin_fabs(c) < RMAX2_D)) {
            a *= RMINSCAL_D;
            b *= RMINSCAL_D;
            c *= RMINSCAL_D;
            d *= RMINSCAL_D;
        }
        ratio = d / c;
        denom = d * ratio + c;
        if (__builtin_fabs(ratio) > RMIN_D) {
            x = (b * ratio + a) / denom;
            y = (b - a * ratio) / denom;
        } else {
            x = (a + d * (b / c)) / denom;
            y = (b - d * (a / c)) / denom;
        }
    }
    if (__builtin_isnan(x) && __builtin_isnan(y)) {
        if (c == 0.0 && d == 0.0 && (!__builtin_isnan(a) || !__builtin_isnan(b))) {
            x = __builtin_copysign(INFINITY_D, c) * a;
            y = __builtin_copysign(INFINITY_D, c) * b;
        } else if ((__builtin_isinf(a) || __builtin_isinf(b)) && __builtin_isfinite(c) &&
                   __builtin_isfinite(d)) {
            a = BOX_D(a);
            b = BOX_D(b);
            x = INFINITY_D * (a * c + b * d);
            y = INFINITY_D * (b * c - a * d);
        } else if ((__builtin_isinf(c) || __builtin_isinf(d)) && __builtin_isfinite(a) &&
                   __builtin_isfinite(b)) {
            c = BOX_D(c);
            d = BOX_D(d);
            x = 0.0 * (a * c + b * d);
            y = 0.0 * (b * c - a * d);
        }
    }
    complex_double r;
    r.part[0] = x;
    r.part[1] = y;
    return r.z;
}

#if __LDBL_MANT_DIG__ == 64 || __LDBL_MANT_DIG__ == 113

#if __LDBL_MANT_DIG__ == 64
#define MUL_L __mulxc3
#define DIV_L __divxc3
#else
#define MUL_L __multc3
#define DIV_L __divtc3
#endif

#define RBIG_L (__LDBL_MAX__ / 2.0L)
#define RMIN_L __LDBL_MIN__
#define RMIN2_L __LDBL_EPSILON__
#define RMINSCAL_L (1.0L / __LDBL_EPSILON__)
#define RMAX2_L (RBIG_L * RMIN2_L)

long double _Complex MUL_L(long double a, long double b, long double c, long double d) {
    long double ac = a * c, bd = b * d, ad = a * d, bc = b * c;
    long double x = ac - bd, y = ad + bc;
    if (__builtin_isnan(x) && __builtin_isnan(y)) {
        int again = 0;
        if (__builtin_isinf(a) || __builtin_isinf(b)) {
            a = BOX_L(a);
            b = BOX_L(b);
            if (__builtin_isnan(c)) c = __builtin_copysignl(0.0L, c);
            if (__builtin_isnan(d)) d = __builtin_copysignl(0.0L, d);
            again = 1;
        }
        if (__builtin_isinf(c) || __builtin_isinf(d)) {
            c = BOX_L(c);
            d = BOX_L(d);
            if (__builtin_isnan(a)) a = __builtin_copysignl(0.0L, a);
            if (__builtin_isnan(b)) b = __builtin_copysignl(0.0L, b);
            again = 1;
        }
        if (!again && (__builtin_isinf(ac) || __builtin_isinf(bd) || __builtin_isinf(ad) ||
                       __builtin_isinf(bc))) {
            if (__builtin_isnan(a)) a = __builtin_copysignl(0.0L, a);
            if (__builtin_isnan(b)) b = __builtin_copysignl(0.0L, b);
            if (__builtin_isnan(c)) c = __builtin_copysignl(0.0L, c);
            if (__builtin_isnan(d)) d = __builtin_copysignl(0.0L, d);
            again = 1;
        }
        if (again) {
            x = INFINITY_L * (a * c - b * d);
            y = INFINITY_L * (a * d + b * c);
        }
    }
    complex_long_double r;
    r.part[0] = x;
    r.part[1] = y;
    return r.z;
}

long double _Complex DIV_L(long double a, long double b, long double c, long double d) {
    long double ratio, denom, x, y;
    if (__builtin_fabsl(c) < __builtin_fabsl(d)) {
        if (__builtin_fabsl(d) >= RBIG_L) {
            a /= 2;
            b /= 2;
            c /= 2;
            d /= 2;
        }
        if (__builtin_fabsl(d) < RMIN2_L ||
            (((__builtin_fabsl(a) < RMIN_L && __builtin_fabsl(b) < RMAX2_L) ||
              (__builtin_fabsl(b) < RMIN_L && __builtin_fabsl(a) < RMAX2_L)) &&
             __builtin_fabsl(d) < RMAX2_L)) {
            a *= RMINSCAL_L;
            b *= RMINSCAL_L;
            c *= RMINSCAL_L;
            d *= RMINSCAL_L;
        }
        ratio = c / d;
        denom = c * ratio + d;
        if (__builtin_fabsl(ratio) > RMIN_L) {
            x = (a * ratio + b) / denom;
            y = (b * ratio - a) / denom;
        } else {
            x = (c * (a / d) + b) / denom;
            y = (c * (b / d) - a) / denom;
        }
    } else {
        if (__builtin_fabsl(c) >= RBIG_L) {
            a /= 2;
            b /= 2;
            c /= 2;
            d /= 2;
        }
        if (__builtin_fabsl(c) < RMIN2_L ||
            (((__builtin_fabsl(a) < RMIN_L && __builtin_fabsl(b) < RMAX2_L) ||
              (__builtin_fabsl(b) < RMIN_L && __builtin_fabsl(a) < RMAX2_L)) &&
             __builtin_fabsl(c) < RMAX2_L)) {
            a *= RMINSCAL_L;
            b *= RMINSCAL_L;
            c *= RMINSCAL_L;
            d *= RMINSCAL_L;
        }
        ratio = d / c;
        denom = d * ratio + c;
        if (__builtin_fabsl(ratio) > RMIN_L) {
            x = (b * ratio + a) / denom;
            y = (b - a * ratio) / denom;
        } else {
            x = (a + d * (b / c)) / denom;
            y = (b - d * (a / c)) / denom;
        }
    }
    if (__builtin_isnan(x) && __builtin_isnan(y)) {
        if (c == 0.0L && d == 0.0L && (!__builtin_isnan(a) || !__builtin_isnan(b))) {
            x = __builtin_copysignl(INFINITY_L, c) * a;
            y = __builtin_copysignl(INFINITY_L, c) * b;
        } else if ((__builtin_isinf(a) || __builtin_isinf(b)) && __builtin_isfinite(c) &&
                   __builtin_isfinite(d)) {
            a = BOX_L(a);
            b = BOX_L(b);
            x = INFINITY_L * (a * c + b * d);
            y = INFINITY_L * (b * c - a * d);
        } else if ((__builtin_isinf(c) || __builtin_isinf(d)) && __builtin_isfinite(a) &&
                   __builtin_isfinite(b)) {
            c = BOX_L(c);
            d = BOX_L(d);
            x = 0.0L * (a * c + b * d);
            y = 0.0L * (b * c - a * d);
        }
    }
    complex_long_double r;
    r.part[0] = x;
    r.part[1] = y;
    return r.z;
}

#endif
