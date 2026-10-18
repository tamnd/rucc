/* reject: all */
/* message: specified mode too small for enumerated values */
/* A `mode` on an enumeration's definition picks how wide it is stored, and 300 does not fit in a
   byte whichever signedness the byte has. gcc refuses it in the same words rather than storing a
   value the enumeration was declared to hold in a type that cannot hold it. */

enum wide { wide_low, wide_high = 300 } __attribute__((mode(byte)));

enum wide f(void) { return wide_high; }
