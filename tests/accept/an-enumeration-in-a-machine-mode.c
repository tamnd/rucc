/* accept: all */
/* A `mode` on an enumeration's definition says how wide the enumeration is stored, and the
   values say whether it is signed, which is how gcc's `finish_enum` reads it. The kernel's netfs
   trace headers write `__mode(byte)` on every enumeration a trace record keeps, and each of those
   records is laid out with one byte for it. */

enum trace { trace_read, trace_write, trace_done } __attribute__((mode(byte)));
enum below { below_low = -1, below_high = 100 } __attribute__((__mode__(__QI__)));
enum half { half_low, half_high = 60000 } __attribute__((mode(HI)));

struct record {
    enum trace what;
    char tag;
};

typedef int widths[sizeof(enum trace) == 1 && sizeof(enum below) == 1 && sizeof(enum half) == 2
                       ? 1
                       : -1];
typedef int laid_out[sizeof(struct record) == 2 ? 1 : -1];

/* The values ask for the signedness: none is negative in `trace` and `half`, and one is in
   `below`, so all ones reads back as the largest value in the first two and as minus one in the
   last. */
typedef int signedness[(enum trace) - 1 > 0 && (enum half) - 1 == 65535 && (enum below) - 1 < 0
                           ? 1
                           : -1];

enum trace f(struct record *r) { return r->what; }
