/* The atomics on an object too wide for the machine to reach in one instruction, which is the
 * routines libatomic exports and the table of locks under them.
 *
 * Design: spec/12-abi-and-runtime.md section 12.8, and tamnd/rucc#1064. The front end refuses an
 * _Atomic type wider than one instruction today, spec/07-types-and-semantics.md says so and says
 * the refusal goes away when a lock table ships, and this is the lock table. The refusal is still
 * there after this file: what lands here is the library half, and the compiler half that turns a
 * wide access into a call to one of these is its own change.
 *
 * The names and the argument order are libatomic's, because an object we produced gets linked
 * against objects GCC produced and there is one atomic object in a program. Two sets of them. The
 * four without a width take the size as their first argument and everything else through a
 * pointer, which is what makes them one routine each rather than one per width: the value never
 * goes in a register, so the same code serves a seventeen byte structure and a __int128. The ones
 * with a width in their name take the value itself and answer with one, which is only possible at
 * a width the machine has a pair of registers for, and there is one such width above the lock free
 * width so the set below is the sixteen byte one.
 *
 * Both sets are here rather than in two files because they have to share the table. A sixteen byte
 * object can arrive at either one: a compiler is free to emit the generic call for it and gcc emits
 * the sized call, and a program built out of two objects can have done both. Two tables would make
 * that the hazard below with the two libraries being the two halves of this file.
 *
 * # The lock, and what it does not give
 *
 * An access here is a lock, a copy of a few bytes, and an unlock. That is what libatomic does and
 * there is nothing else available: a machine with no instruction at this width has no way to make
 * the access indivisible except by agreement between everybody who touches the object.
 *
 * Which is the hazard, and it is worth writing down rather than leaving for somebody to find. Two
 * pieces of a program only agree if they take the same lock, and the lock lives in this file. A
 * program half of whose accesses come through here and half of which go through the libatomic GCC
 * linked in has two tables and two locks for one object, and is not atomic at all. The same is
 * true of GCC's libatomic against LLVM's compiler-rt today, so this is the state of the world
 * rather than something new here, and the way out of it is the ordinary one: one library on the
 * link line.
 *
 * # What the ordering argument does
 *
 * Nothing. Every one of the four takes one and none of them reads it. The lock is an acquire and
 * the unlock is a release, so two accesses to the same object are ordered against each other
 * whatever either of them asked for, and a caller that asked for less than that has been given
 * more, which it is allowed to be. libatomic ignores the argument here for the same reason.
 */

#include <stddef.h>

/* An integer as wide as a pointer. From the compiler rather than from stdint.h, because this is
 * built freestanding and the header is not promised there.
 */
typedef __UINTPTR_TYPE__ uptr;

/* How many locks there are, as the number of bits the hash keeps. Sixty four entries: small
 * enough that the table is one page of zeroed memory and large enough that two unrelated objects
 * meeting in it is rare. libatomic picks the same number.
 */
#define LOCK_BITS 6
#define LOCKS (1 << LOCK_BITS)

/* How far apart they sit, which is a cache line on every target in the matrix. Two threads
 * holding different locks should not be writing to the same line, since that would hand the line
 * back and forth between them for no reason at all.
 */
#define LINE 64

struct guard {
    /* Zero when free and one when held. A byte rather than a word because the exchange below is
     * an instruction at every width on every target, and a byte is the width with no alignment to
     * arrange for.
     */
    unsigned char held;
    unsigned char padding[LINE - 1];
};

static struct guard table[LOCKS];

/* Which entry an object uses, decided by its address and by nothing else.
 *
 * The multiply is the whole of it. An object wide enough to reach these routines is aligned to
 * eight bytes at least and usually to sixteen, so the low bits of every address this sees are
 * zero, and reading the entry straight off the address would put a whole array of atomic objects
 * onto one entry and leave the rest of the table empty. A multiply by an odd number carries what
 * is above those bits up into the top of the word, and the top six bits of the product are the
 * entry. The constant is the odd number nearest the golden ratio at sixty four bits, which is
 * truncated to the low half on a target whose pointers are narrower and stays odd there, and odd
 * is all this asks of it.
 *
 * One lock for the whole object, taken at the address the caller passed. libatomic hashes by
 * cache line instead and takes every lock the object spans, which is what lets it handle an
 * access to part of an atomic object. There is no such access in C: an _Atomic object is read and
 * written whole and there is no way to name a piece of one, so every call about a given object
 * arrives with the same address and lands on the same entry. The simpler rule also cannot
 * deadlock, which the other one can in principle, since it never holds two locks at once.
 */
static struct guard *guard_for(const void *object) {
    uptr mixed = (uptr)object * (uptr)0x9E3779B97F4A7C15ull;
    return &table[mixed >> (sizeof(uptr) * 8 - LOCK_BITS)];
}

/* Takes the lock, by spinning. Not by asking a scheduler for anything, because what runs under it
 * is a copy of a few bytes and the wait is shorter than the call into the kernel that would avoid
 * it, and because half the targets in the matrix have no kernel to call.
 */
static void take(struct guard *guard) {
    while (__atomic_exchange_n(&guard->held, 1, __ATOMIC_ACQUIRE) != 0) {
    }
}

static void drop(struct guard *guard) {
    __atomic_store_n(&guard->held, 0, __ATOMIC_RELEASE);
}

/* A byte at a time, written here rather than called out to memcpy in mem.c. Two reasons, and the
 * second is the one that matters: a routine holding a spin lock should not reach a name a program
 * is allowed to replace with its own, and the ranges these copy between are a few bytes long and
 * would spend more time in the call than in the loop.
 */
static void copy(void *to, const void *from, size_t count) {
    unsigned char *out = to;
    const unsigned char *in = from;
    while (count != 0) {
        *out = *in;
        out += 1;
        in += 1;
        count -= 1;
    }
}

/* Whether two ranges hold the same bytes. The comparison the exchange below makes is over the
 * object's representation and not over its value, which is what the standard says and is why a
 * structure with padding in it compares unequal to a copy of itself that was built a different
 * way. gcc compares representations here too.
 */
static int same(const void *one, const void *two, size_t count) {
    const unsigned char *left = one;
    const unsigned char *right = two;
    while (count != 0) {
        if (*left != *right) {
            return 0;
        }
        left += 1;
        right += 1;
        count -= 1;
    }
    return 1;
}

/* The names are given by an assembler label rather than written as the function's own, because
 * __atomic_load and the other three are type generic builtins in both this compiler and gcc: a
 * declaration of one with a prototype is a declaration that disagrees with the builtin, and a
 * call to one with four arguments is a call the front end reads as the builtin with the wrong
 * number of arguments. libatomic has the same problem and solves it the same way, by defining
 * libat_load and aliasing the symbol.
 */
void generic_load(size_t size, void *object, void *into, int order) __asm__("__atomic_load");
void generic_store(size_t size, void *object, void *value, int order) __asm__("__atomic_store");
void generic_exchange(size_t size, void *object, void *value, void *into,
                      int order) __asm__("__atomic_exchange");
_Bool generic_compare_exchange(size_t size, void *object, void *expected, void *desired,
                               int success, int failure) __asm__("__atomic_compare_exchange");

void generic_load(size_t size, void *object, void *into, int order) {
    struct guard *guard = guard_for(object);
    (void)order;
    take(guard);
    copy(into, object, size);
    drop(guard);
}

void generic_store(size_t size, void *object, void *value, int order) {
    struct guard *guard = guard_for(object);
    (void)order;
    take(guard);
    copy(object, value, size);
    drop(guard);
}

/* The old value comes out before the new one goes in, which is the order libatomic writes it in
 * and is the order that gives a caller who passed the same buffer for both an exchange with
 * itself rather than a buffer full of what it already had.
 */
void generic_exchange(size_t size, void *object, void *value, void *into, int order) {
    struct guard *guard = guard_for(object);
    (void)order;
    take(guard);
    copy(into, object, size);
    copy(object, value, size);
    drop(guard);
}

/* The write back on failure is the reason the expected value arrives by pointer. A caller that
 * lost the race gets what was actually there, so the loop it is almost certainly sitting in can
 * go round again without reading the object a second time.
 *
 * Two orderings arrive and neither is read, for the reason at the top of the file. The one for
 * the failing case is the one a caller is most likely to have written as relaxed, and relaxed is
 * not what it gets here, which costs it nothing it can measure.
 */
_Bool generic_compare_exchange(size_t size, void *object, void *expected, void *desired,
                               int success, int failure) {
    struct guard *guard = guard_for(object);
    int matched;
    (void)success;
    (void)failure;
    take(guard);
    matched = same(object, expected, size);
    if (matched) {
        copy(object, desired, size);
    } else {
        copy(expected, object, size);
    }
    drop(guard);
    return matched != 0;
}

/* The set with a width in the name, which is the sixteen byte one and is the only one of them
 * worth having here. A width the machine reaches in one instruction never becomes a call, and the
 * widths above sixteen have no integer type to carry a value in and go through the four above.
 *
 * The guard is only around the type, which the compiler has on every target in the matrix today.
 * It is what says whether the width exists at all, and a target without it has no sixteen byte
 * object for these to be about.
 */
#ifdef __SIZEOF_INT128__

/* Unsigned because every operation below is over bits rather than over numbers: the six are
 * addition, subtraction and the four bitwise ones, and the only one of those that reads a sign is
 * a shift, which is not one of them. So the type is the one with no overflow to think about.
 */
typedef unsigned __int128 u128;

/* The object is reached through an ordinary pointer of the type rather than a byte at a time. It
 * is the width and the alignment of the type by construction, since nothing else is sixteen bytes
 * and atomic, so the access is what the machine does with two registers and the lock is what makes
 * the pair of them one access.
 *
 * The volatile on the parameters is libatomic's and it is dropped on the way in. A qualifier says
 * what the compiler may do between two accesses in one thread, which is a question about the
 * caller's code and not about these six lines, and what makes the access here indivisible is the
 * lock either way.
 */
u128 __atomic_load_16(const volatile void *object, int order) {
    struct guard *guard = guard_for((const void *)object);
    u128 was;
    (void)order;
    take(guard);
    was = *(const u128 *)object;
    drop(guard);
    return was;
}

void __atomic_store_16(volatile void *object, u128 value, int order) {
    struct guard *guard = guard_for((const void *)object);
    (void)order;
    take(guard);
    *(u128 *)object = value;
    drop(guard);
}

u128 __atomic_exchange_16(volatile void *object, u128 value, int order) {
    struct guard *guard = guard_for((const void *)object);
    u128 *at = (u128 *)object;
    u128 was;
    (void)order;
    take(guard);
    was = *at;
    *at = value;
    drop(guard);
    return was;
}

/* The expected value arrives by pointer here as it does in the generic one, and for the same
 * reason: what was there is written back over it when the exchange did not happen.
 *
 * The comparison is over the value and not over the representation, which is the one place this
 * set and the generic one differ and is not a choice either of them made. An object with a value
 * in a register has no padding left to compare, and sixteen bytes of integer has none to begin
 * with, so the two answer the same question by different means.
 *
 * A weak exchange is allowed to fail with the value it was asked for sitting in the object, and
 * this one never does. A caller that asked for the weak one is in a loop that goes round again,
 * so the strong answer costs it nothing and is the answer libatomic gives it too.
 */
_Bool __atomic_compare_exchange_16(volatile void *object, void *expected, u128 desired, _Bool weak,
                                   int success, int failure) {
    struct guard *guard = guard_for((const void *)object);
    u128 *at = (u128 *)object;
    u128 *want = (u128 *)expected;
    u128 was;
    int matched;
    (void)weak;
    (void)success;
    (void)failure;
    take(guard);
    was = *at;
    matched = was == *want;
    if (matched) {
        *at = desired;
    } else {
        *want = was;
    }
    drop(guard);
    return matched != 0;
}

/* The six that read, work on what they read and write it back, and answer what was there before.
 *
 * The six beside them that answer the value afterwards are each one of these and then the same
 * operation again over the answer, which is what gcc does when it lowers one of those names onto
 * the other, and it is right here for the reason it is right there: the value afterwards is a
 * function of the value before and the operand, both of which are in registers by then, and
 * working it out costs an instruction rather than a second trip through the lock.
 */
u128 __atomic_fetch_add_16(volatile void *object, u128 value, int order) {
    struct guard *guard = guard_for((const void *)object);
    u128 *at = (u128 *)object;
    u128 was;
    (void)order;
    take(guard);
    was = *at;
    *at = was + value;
    drop(guard);
    return was;
}

u128 __atomic_fetch_sub_16(volatile void *object, u128 value, int order) {
    struct guard *guard = guard_for((const void *)object);
    u128 *at = (u128 *)object;
    u128 was;
    (void)order;
    take(guard);
    was = *at;
    *at = was - value;
    drop(guard);
    return was;
}

u128 __atomic_fetch_and_16(volatile void *object, u128 value, int order) {
    struct guard *guard = guard_for((const void *)object);
    u128 *at = (u128 *)object;
    u128 was;
    (void)order;
    take(guard);
    was = *at;
    *at = was & value;
    drop(guard);
    return was;
}

u128 __atomic_fetch_or_16(volatile void *object, u128 value, int order) {
    struct guard *guard = guard_for((const void *)object);
    u128 *at = (u128 *)object;
    u128 was;
    (void)order;
    take(guard);
    was = *at;
    *at = was | value;
    drop(guard);
    return was;
}

u128 __atomic_fetch_xor_16(volatile void *object, u128 value, int order) {
    struct guard *guard = guard_for((const void *)object);
    u128 *at = (u128 *)object;
    u128 was;
    (void)order;
    take(guard);
    was = *at;
    *at = was ^ value;
    drop(guard);
    return was;
}

/* The odd one out of the six, which is an and and then a complement rather than one operator.
 * There is no such operator in C and there is no instruction for it on most machines either, and
 * it is in the family because the kernel uses it to clear a bit and answer what was there.
 */
u128 __atomic_fetch_nand_16(volatile void *object, u128 value, int order) {
    struct guard *guard = guard_for((const void *)object);
    u128 *at = (u128 *)object;
    u128 was;
    (void)order;
    take(guard);
    was = *at;
    *at = ~(was & value);
    drop(guard);
    return was;
}

u128 __atomic_add_fetch_16(volatile void *object, u128 value, int order) {
    return __atomic_fetch_add_16(object, value, order) + value;
}

u128 __atomic_sub_fetch_16(volatile void *object, u128 value, int order) {
    return __atomic_fetch_sub_16(object, value, order) - value;
}

u128 __atomic_and_fetch_16(volatile void *object, u128 value, int order) {
    return __atomic_fetch_and_16(object, value, order) & value;
}

u128 __atomic_or_fetch_16(volatile void *object, u128 value, int order) {
    return __atomic_fetch_or_16(object, value, order) | value;
}

u128 __atomic_xor_fetch_16(volatile void *object, u128 value, int order) {
    return __atomic_fetch_xor_16(object, value, order) ^ value;
}

u128 __atomic_nand_fetch_16(volatile void *object, u128 value, int order) {
    return ~(__atomic_fetch_nand_16(object, value, order) & value);
}

#endif
