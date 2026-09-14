/* Arithmetic on a `_Float128`, run rather than read.
 *
 * Design: spec/12-abi-and-runtime.md section 12.8. The pass this is about is
 * crates/rucc-codegen/src/quad.rs, which turns every operation at this format into the call that
 * performs it, because no machine has an instruction that adds two binary128 values. Its own tests
 * read the IR it produced, which is the same kind of evidence as a stub writer reading its own bytes
 * back, so what this file is for is the other kind: the same program compiled by this compiler and
 * by the system compiler, run, and the two outputs held against each other.
 *
 * It is the second fixture of this shape and tests/wide/arithmetic.c is the first, so everything
 * about how a group is printed and why the inputs are built rather than sampled from a library is
 * written down there.
 *
 * What is different here is the hazard list. An integer has edges at the ends of its type and at the
 * boundary between the halves, and a float has edges everywhere: a subnormal, a zero with a sign on
 * it, an infinity, a not a number with a payload, and a value exactly half way between two of the
 * next format down. So the random cases put the exponent where the operation has a decision to make
 * rather than leaving it to thirty two thousand equally likely values, and the corner table is read
 * at both signs.
 *
 * Nothing here is undefined. A division by zero at a floating point type is an infinity rather than
 * undefined behaviour, so there is nothing to avoid in the arithmetic. The conversions coming down
 * to an integer are the one place with a rule: a value whose integer part the type cannot hold is
 * undefined, so those values are built inside the range rather than tested there and excluded
 * afterwards.
 */

/* Declared rather than included, so that both sides of the comparison are reading the same
 * declaration and neither is reading a header this compiler ships and the other one does not.
 */
int printf(const char *format, ...);
void *memcpy(void *to, const void *from, unsigned long size);

typedef unsigned int u32;
typedef unsigned long long u64;

/* The widest integer, which the conversions at the end of `integers` are the only use of. Both
 * compilers this fixture is built with have the type on this machine, so it is spelled here without a
 * guard the way the rest of the file spells a `long long`.
 */
typedef __int128 i128;
typedef unsigned __int128 u128;

/* How many rounds of random cases, and how many cases in one. The product is what the cases number
 * in the output is, and the split into rounds is so that a difference says which round it started
 * at, which is what makes a run reproducible from the seed.
 */
#define ROUNDS 8
#define CASES 1024

/* How many cases ran, printed at the end so that a run that stopped early is a different thing from
 * a run that disagreed.
 */
static long cases = 0;

/* xorshift64, seeded away from the one state it cannot leave. */
static u64 state = 0x9E3779B97F4A7C15ull;

static u64 next_random(void) {
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    return state;
}

/* The two words of a quad in memory, low one first, which is this machine's order. */
static _Float128 quad_of(u64 high, u64 low) {
    u64 words[2];
    _Float128 value;
    words[0] = low;
    words[1] = high;
    memcpy(&value, words, sizeof value);
    return value;
}

/* FNV-1a over the sixteen bytes of a value.
 *
 * Over the bytes rather than over the number, because what an operation at this format rounded to is
 * a question about bits: two values that print the same may be a rounding apart, and a not a number
 * carries a payload that no comparison can see.
 */
static u64 mix_quad(u64 digest, _Float128 value) {
    unsigned char bytes[16];
    memcpy(bytes, &value, sizeof bytes);
    for (int i = 0; i < 16; i++) {
        digest ^= bytes[i];
        digest *= 1099511628211ull;
    }
    return digest;
}

static u64 mix_word(u64 digest, u64 value) {
    for (int i = 0; i < 8; i++) {
        digest ^= (unsigned char)(value >> (8 * i));
        digest *= 1099511628211ull;
    }
    return digest;
}

/* Both words of the widest integer, low one first, so that an answer that is right in one word and
 * wrong in the other is a difference here rather than a digest that happens to agree.
 */
static u64 mix_wide(u64 digest, u128 value) {
    digest = mix_word(digest, (u64)value);
    return mix_word(digest, (u64)(value >> 64));
}

static u64 mix_double(u64 digest, double value) {
    u64 bits;
    memcpy(&bits, &value, sizeof bits);
    return mix_word(digest, bits);
}

static u64 mix_float(u64 digest, float value) {
    u32 bits;
    memcpy(&bits, &value, sizeof bits);
    return mix_word(digest, (u64)bits);
}

/* The exponent field with every bit set, and the significand under it. Both set is a not a number. */
#define QUAD_EXPONENT 0x7FFF000000000000ull
#define QUAD_PAYLOAD 0x0000FFFFFFFFFFFFull

/* What a not a number digests as instead of its bits. Any word will do as long as it is one no pair
 * of quad words can produce, and eight bytes cannot collide with the sixteen a value digests as. */
#define NOT_A_NUMBER 0x4E6F744E756D6265ull

/* FNV-1a over an answer, counting a not a number as one value rather than as the bits it came back
 * with.
 *
 * The two sides of this differential do not agree about which not a number an operation produces,
 * and that is a decision rather than a disagreement about arithmetic. libgcc answers two questions
 * out of a per architecture header: which operand comes back when both of them are not a numbers,
 * and what an operation with no answer at all invents. It takes the right operand on x86 and the
 * left one on arm, and it invents a set sign on one and a clear one on the other.
 * `runtime/builtins/quad.c` serves every row of the target matrix out of one archive, so it has one
 * rule instead of a rule per row, and `spec/12-abi-and-runtime.md` section 12.8 says which rule and
 * why it is that one. Against the x86 libgcc this runs next to, the two rules part company at the
 * corners of the table where an operation has two not a numbers in front of it or no answer at all,
 * and nowhere else in this fixture.
 *
 * So the table asks the question it can ask. Both sides have to agree that the answer is a not a
 * number rather than a value, and about every bit of every answer that is a value, which is what a
 * wrong routine or a pair of operands the wrong way round would break. Which not a number is checked
 * a layer down by `cargo xtask builtins-diff`, against a reference that carries the same rule on
 * purpose.
 */
static u64 mix_answer(u64 digest, _Float128 value) {
    u64 words[2];
    memcpy(words, &value, sizeof words);
    if ((words[1] & QUAD_EXPONENT) == QUAD_EXPONENT && ((words[1] & QUAD_PAYLOAD) | words[0]) != 0) {
        return mix_word(digest, NOT_A_NUMBER);
    }
    return mix_quad(digest, value);
}

/* A random quad, every bit of it, which lands on a not a number or an infinity once in thirty two
 * thousand and on a subnormal as often as that.
 */
static _Float128 random_quad(void) {
    /* Two statements rather than two arguments, because C does not say which argument of a call is
     * evaluated first and the two compilers here do not agree: one built the value out of the
     * random words one way round and the other built it the other way round, which is two different
     * programs and not two answers to the same question. It cost a run of this fixture to find.
     */
    u64 high = next_random();
    u64 low = next_random();
    return quad_of(high, low);
}

/* A random quad whose stored exponent is this one, which is how a case reaches the band an operation
 * has a decision in. Everything below the exponent is random, the sign included.
 */
static _Float128 quad_at(u64 stored) {
    u64 high = next_random();
    high = (high & 0x8000FFFFFFFFFFFFull) | (stored << 48);
    return quad_of(high, next_random());
}

/* Two quads whose exponents are within a few of each other, which is the pair a subtraction
 * cancels and the pair an addition has to align rather than discard.
 */
static u64 near_one(void) {
    return 16000 + next_random() % 768;
}

static void report(const char *what, int round, u64 digest) {
    printf("%s %d %llu\n", what, round, digest);
}

/* The four operations and the negation, over pairs whose exponents are close enough to interact.
 *
 * Close exponents rather than independent ones, because two random quads are about ten thousand
 * exponents apart and a sum of those is the larger operand with a sticky bit, which is a case
 * nothing gets wrong twice. A pair within a few hundred is where the alignment shift, the
 * cancellation and the rounding all happen.
 */
static void arithmetic(int round) {
    u64 sums = 0, differences = 0, products = 0, quotients = 0, negations = 0;
    for (int i = 0; i < CASES; i++) {
        _Float128 a = quad_at(near_one());
        _Float128 b = quad_at(near_one());
        sums = mix_quad(sums, a + b);
        differences = mix_quad(differences, a - b);
        products = mix_quad(products, a * b);
        quotients = mix_quad(quotients, a / b);
        negations = mix_quad(negations, -a);
        cases += 5;
    }
    report("add", round, sums);
    report("sub", round, differences);
    report("mul", round, products);
    report("div", round, quotients);
    report("neg", round, negations);
}

/* The same five over pairs of fully random patterns, which is where the infinities and the not a
 * numbers come from.
 *
 * A separate group rather than more cases in the one above, because the two are asking different
 * questions: that one is about rounding and this one is about the answers that have no arithmetic in
 * them at all. An infinity less an infinity, a zero over a zero and a not a number through any of
 * the five are all in here, and which of them comes out is libgcc's convention rather than
 * something C says, which is exactly why it is held against libgcc by running.
 */
static void extremes(int round) {
    u64 sums = 0, differences = 0, products = 0, quotients = 0, negations = 0;
    for (int i = 0; i < CASES; i++) {
        _Float128 a = random_quad();
        _Float128 b = random_quad();
        sums = mix_quad(sums, a + b);
        differences = mix_quad(differences, a - b);
        products = mix_quad(products, a * b);
        quotients = mix_quad(quotients, a / b);
        negations = mix_quad(negations, -b);
        cases += 5;
    }
    report("wildadd", round, sums);
    report("wildsub", round, differences);
    report("wildmul", round, products);
    report("wilddiv", round, quotients);
    report("wildneg", round, negations);
}

/* The twelve comparisons, packed a bit each into one number.
 *
 * Six operators and each of them negated, because the negation is not the opposite comparison when
 * an operand is a not a number: every one of the six is false for such a pair and so every negation
 * is true, and the two readings are two different routines in the runtime. A pair of random patterns
 * is a not a number once in sixteen thousand, so half the cases are built at an exponent that makes
 * one on purpose.
 */
static void comparisons(int round) {
    u64 digest = 0;
    for (int i = 0; i < CASES; i++) {
        _Float128 a = (i & 1) ? random_quad() : quad_at(near_one());
        _Float128 b = (i & 2) ? random_quad() : quad_at(near_one());
        u32 bits = 0;
        bits |= (u32)(a == b) << 0;
        bits |= (u32)(a != b) << 1;
        bits |= (u32)(a < b) << 2;
        bits |= (u32)(a <= b) << 3;
        bits |= (u32)(a > b) << 4;
        bits |= (u32)(a >= b) << 5;
        bits |= (u32)(!(a == b)) << 6;
        bits |= (u32)(!(a != b)) << 7;
        bits |= (u32)(!(a < b)) << 8;
        bits |= (u32)(!(a <= b)) << 9;
        bits |= (u32)(!(a > b)) << 10;
        bits |= (u32)(!(a >= b)) << 11;
        /* A value against itself, which is true for every pair but a not a number and is how a
         * program asks whether something is one without a library.
         */
        bits |= (u32)(a == a) << 12;
        bits |= (u32)(b != b) << 13;
        /* The three questions C spells as a macro rather than an operator, which are the ones that
         * reach the routine that answers only whether the two can be ordered at all. An operator
         * never asks that on its own: every one of the six has an answer for an unordered pair
         * already built into which routine it calls.
         */
        bits |= (u32)(__builtin_isunordered(a, b)) << 14;
        bits |= (u32)(__builtin_isnan(a)) << 15;
        bits |= (u32)(__builtin_islessgreater(a, b)) << 16;
        digest = mix_word(digest, (u64)bits);
        cases += 17;
    }
    report("cmp", round, digest);
}

/* The conversions against the two narrower formats, both ways.
 *
 * Coming down, the exponent is put in the band where the narrowing chooses between a finite answer
 * and an infinity and between a subnormal and a zero, which is two hundred and ninety exponents wide
 * for a float and two thousand one hundred and twenty for a double and which random bits reach about
 * never. Going up never rounds, and the input that is not a field move is a subnormal of the narrow
 * format, so one of those is made every time.
 */
static void formats(int round) {
    u64 singles = 0, doubles = 0, up_single = 0, up_double = 0, through = 0;
    for (int i = 0; i < CASES; i++) {
        _Float128 near_single = quad_at(16230 + next_random() % 290);
        _Float128 near_double = quad_at(15300 + next_random() % 2120);
        singles = mix_float(singles, (float)near_single);
        doubles = mix_double(doubles, (double)near_double);

        u32 single_bits = (u32)next_random();
        u64 double_bits = next_random();
        float small;
        double wide;
        memcpy(&small, &single_bits, sizeof small);
        memcpy(&wide, &double_bits, sizeof wide);
        up_single = mix_quad(up_single, (_Float128)small);
        up_double = mix_quad(up_double, (_Float128)wide);

        /* A subnormal at each narrow format, and the way down two formats at once, which is how a
         * program that keeps a long double and prints a float gets there.
         */
        single_bits &= 0x807FFFFFu;
        double_bits &= 0x800FFFFFFFFFFFFFull;
        memcpy(&small, &single_bits, sizeof small);
        memcpy(&wide, &double_bits, sizeof wide);
        up_single = mix_quad(up_single, (_Float128)small);
        up_double = mix_quad(up_double, (_Float128)wide);
        through = mix_float(through, (float)(double)near_single);
        cases += 7;
    }
    report("tosingle", round, singles);
    report("todouble", round, doubles);
    report("fromsingle", round, up_single);
    report("fromdouble", round, up_double);
    report("twosteps", round, through);
}

/* The conversions against an integer, both ways and at all three widths and both signednesses.
 *
 * Going up, every integer narrower than the widest type is exact at this format, so what those ask
 * is whether the right routine was called rather than how it rounded. Coming down is where the
 * range matters: a value whose integer part the type cannot hold is undefined, so the exponent is
 * kept inside the type and the band starts below one, where the answer is a zero however many bits
 * are under it.
 *
 * Four bands rather than one, because the type the answer lands in is what bounds it: a value under
 * 2^126 is inside the pair at a hundred and twenty eight bits, one under 2^62 is inside every pair
 * at sixty four, one under 2^31 is inside the pair at thirty two, and the narrowest case here is a
 * `short`, which holds less than 2^15.
 */
static void integers(int round) {
    u64 from_signed = 0, from_unsigned = 0, to_signed = 0, to_unsigned = 0, narrow = 0;
    u64 from_widest = 0, to_widest = 0;
    for (int i = 0; i < CASES; i++) {
        u64 bits = next_random();
        int small = (int)bits;
        unsigned int small_unsigned = (unsigned int)bits;
        long long big = (long long)bits;
        unsigned long long big_unsigned = bits;
        from_signed = mix_quad(from_signed, (_Float128)small + (_Float128)big);
        from_unsigned =
            mix_quad(from_unsigned, (_Float128)small_unsigned + (_Float128)big_unsigned);

        /* An eighth up to just under 2^62, which every type at sixty four bits holds at either
         * sign, and the same band stopped where a thirty two bit type and a `short` hold it.
         */
        _Float128 value = quad_at(16380 + next_random() % 65);
        _Float128 small_value = quad_at(16380 + next_random() % 34);
        _Float128 tiny_value = quad_at(16380 + next_random() % 17);
        to_signed = mix_word(to_signed, (u64)(long long)value);
        to_unsigned = mix_word(to_unsigned, (u64)(unsigned long long)(value < 0 ? -value : value));
        narrow = mix_word(narrow, (u64)(u32)(int)small_value);
        narrow = mix_word(narrow, (u64)(unsigned int)(small_value < 0 ? -small_value : small_value));
        narrow = mix_word(narrow, (u64)(unsigned char)(short)tiny_value);

        /* And the same both ways at the widest type, which is the one place in this family where
         * the answer going up is a rounding of the value rather than the value: a hundred and
         * thirteen significant bits hold every integer the types above deal in and do not hold
         * every one of these. The operand is a random word pair with a random number of its top
         * bits gone, so that both sides of that are asked.
         */
        u128 widest = (((u128)next_random() << 64) | next_random()) >> (next_random() % 128);
        from_widest = mix_quad(from_widest, (_Float128)(i128)widest);
        from_widest = mix_quad(from_widest, (_Float128)widest);
        _Float128 wide_value = quad_at(16380 + next_random() % 129);
        to_widest = mix_wide(to_widest, (u128)(i128)wide_value);
        to_widest = mix_wide(to_widest, (u128)(wide_value < 0 ? -wide_value : wide_value));
        cases += 12;
    }
    report("fromint", round, from_signed);
    report("fromuint", round, from_unsigned);
    report("fromwide", round, from_widest);
    report("toint", round, to_signed);
    report("touint", round, to_unsigned);
    report("towide", round, to_widest);
    report("tonarrow", round, narrow);
}

/* The patterns worth naming, every operation over every pair of them at both signs.
 *
 * A random case reaches a zero, an infinity or a payload that is exactly one bit about as often as
 * it reaches any other pattern, which is never, and these are where the answers are decided.
 */
static const u64 CORNER_HIGH[] = {
    0x0000000000000000ull, /* zero */
    0x0000000000000001ull, /* the smallest subnormal */
    0x0000FFFFFFFFFFFFull, /* the largest subnormal */
    0x0001000000000000ull, /* the smallest normal */
    0x3FFF000000000000ull, /* one */
    0x3FFE000000000000ull, /* a half */
    0x4000000000000000ull, /* two */
    0x7FFEFFFFFFFFFFFFull, /* the largest finite value */
    0x7FFF000000000000ull, /* an infinity */
    0x7FFF800000000000ull, /* a quiet not a number */
    0x7FFF000000000000ull, /* a signalling one, with the payload below */
    0x407E000000000000ull, /* the largest exponent a float holds */
    0x3F81000000000000ull, /* the smallest normal float */
    0x43FE000000000000ull, /* the largest exponent a double holds */
    0x3C01000000000000ull, /* the smallest normal double */
};

static const u64 CORNER_LOW[] = {
    0x0000000000000000ull, 0x0000000000000000ull, 0xFFFFFFFFFFFFFFFFull, 0x0000000000000000ull,
    0x0000000000000000ull, 0x0000000000000000ull, 0x0000000000000000ull, 0xFFFFFFFFFFFFFFFFull,
    0x0000000000000000ull, 0x0000000000000000ull, 0x0000000000000001ull, 0xFFFFFFFFFFFFFFFFull,
    0x0000000000000000ull, 0xFFFFFFFFFFFFFFFFull, 0x0000000000000000ull,
};

#define CORNER_COUNT ((int)(sizeof CORNER_HIGH / sizeof CORNER_HIGH[0]))

/* The sign bit, which doubles the table. */
#define QUAD_SIGN 0x8000000000000000ull

static void corners(void) {
    u64 sums = 0, differences = 0, products = 0, quotients = 0, negations = 0;
    u64 answered = 0, singles = 0, doubles = 0;
    for (int i = 0; i < CORNER_COUNT; i++) {
        for (int j = 0; j < CORNER_COUNT; j++) {
            for (int sign = 0; sign < 4; sign++) {
                /* Through a volatile word, so that the two compilers are doing the same work.
                 *
                 * Every input here is a constant in a table, and an optimizer that sees that is
                 * free to work the answer out at compile time instead of calling anything. One of
                 * these compilers would then be comparing what its own folding believes against
                 * what the other one's runtime computed, which is a different question from the one
                 * this fixture asks and a worse one: the two disagree about the payload of a not a
                 * number long before they disagree about arithmetic. A volatile read has to happen,
                 * so neither side can fold past it and both sides call the routine.
                 */
                volatile u64 left = CORNER_HIGH[i] | ((sign & 1) ? QUAD_SIGN : 0);
                volatile u64 right = CORNER_HIGH[j] | ((sign & 2) ? QUAD_SIGN : 0);
                volatile u64 below = CORNER_LOW[i];
                volatile u64 above = CORNER_LOW[j];
                _Float128 a = quad_of(left, below);
                _Float128 b = quad_of(right, above);
                u32 answers = 0;
                answers |= (u32)(a == b) << 0;
                answers |= (u32)(a < b) << 1;
                answers |= (u32)(a <= b) << 2;
                answers |= (u32)(a > b) << 3;
                answers |= (u32)(a >= b) << 4;
                answers |= (u32)(a != b) << 5;
                /* The same six written as negations, which is the only way a C program reaches the
                 * four predicates that are true for a pair with no order between them. Unordered or
                 * less than is not at or above, and the three below it are the same trick, and an
                 * optimizer is what folds the pair into one predicate: at the level where nothing
                 * folds these stay a comparison and a negation and the answer is the same either
                 * way. The table is where they belong rather than the random family, because a pair
                 * with no order between it needs a not a number in it, and this is where the not a
                 * numbers are.
                 */
                answers |= (u32)(!(a >= b)) << 6;
                answers |= (u32)(!(a > b)) << 7;
                answers |= (u32)(!(a <= b)) << 8;
                answers |= (u32)(!(a < b)) << 9;
                /* One digest per operation rather than one for the table, so that a difference
                 * here says which operation it is in. The random groups above are split that way
                 * for the same reason, and this is where it matters most: every pattern in the
                 * table is one somebody chose, so a difference in one operation over one of them is
                 * a sentence about that operation rather than a number to go hunting with.
                 */
                sums = mix_answer(sums, a + b);
                differences = mix_answer(differences, a - b);
                products = mix_answer(products, a * b);
                quotients = mix_answer(quotients, a / b);
                /* The negation digests its whole answer, unlike the four above it. Flipping a sign
                 * bit is not one of the two things the two sides decide differently: a negated not a
                 * number is the operand with one bit turned over, which both sides agree about, and
                 * a rule that let this one through would stop the table from checking the one bit
                 * the operation touches.
                 */
                negations = mix_quad(negations, -a);
                answered = mix_word(answered, (u64)answers);
                singles = mix_float(singles, (float)a);
                doubles = mix_double(doubles, (double)a);
                cases += 8;
            }
        }
    }
    report("cornadd", 0, sums);
    report("cornsub", 0, differences);
    report("cornmul", 0, products);
    report("corndiv", 0, quotients);
    report("cornneg", 0, negations);
    report("corncmp", 0, answered);
    report("cornsingle", 0, singles);
    report("corndouble", 0, doubles);
}

/* A constant at this format, which is the one value in the program the compiler has to write down
 * rather than compute.
 *
 * No instruction carries sixteen bytes of immediate, so the bits go somewhere the program can read
 * them from, and what this asks is whether they arrived. The three shapes are a zero, a value that
 * is exact one format down, and one that is not: a third is the constant a program writes without
 * thinking about it, and the last is the one where a compiler that kept the value as a double and
 * widened it afterwards would be one rounding out.
 *
 * The literals below carry the suffix that makes them this format, and that is the whole point of
 * them rather than a spelling preference. `0.5` is a double, and a double assigned to a `_Float128`
 * is a widening, which is a call and not a constant, so a fixture written without the suffix asks
 * about `__extenddftf2` twice and about a sixteen byte constant never. `0.5f128` is the value
 * itself, and `0.1f128` is the one that tells the two apart: the nearest double to a tenth widened
 * to this format is not the nearest quad to a tenth, so a compiler that read the decimal at the
 * narrower width and widened afterwards prints a different number here.
 */
static void constants(void) {
    u64 digest = 0;
    _Float128 zero = 0;
    _Float128 half = 0.5f128;
    _Float128 tenth = 0.1f128;
    _Float128 many = 1.2345678901234567890123456789012345f128;
    _Float128 third = (_Float128)1 / 3;
    _Float128 tiny = (_Float128)1 / 0x1000000000000000ull / 0x1000000000000000ull;
    digest = mix_quad(digest, zero);
    digest = mix_quad(digest, half);
    digest = mix_quad(digest, tenth);
    digest = mix_quad(digest, many);
    digest = mix_quad(digest, third);
    digest = mix_quad(digest, tiny);
    digest = mix_quad(digest, zero - half);
    digest = mix_quad(digest, tenth + many);
    digest = mix_quad(digest, third * third);
    digest = mix_quad(digest, tiny * tiny);
    digest = mix_quad(digest, 1 / zero);
    digest = mix_word(digest, (u64)(zero == -zero));
    digest = mix_word(digest, (u64)(long long)(third * 3));
    digest = mix_word(digest, (u64)(long long)(many * 1000000000000000000.0f128));
    cases += 14;
    report("constant", 0, digest);
}

int main(void) {
    for (int round = 0; round < ROUNDS; round++) {
        arithmetic(round);
        extremes(round);
        comparisons(round);
        formats(round);
        integers(round);
    }
    corners();
    constants();
    printf("cases %ld\n", cases);
    return 0;
}
