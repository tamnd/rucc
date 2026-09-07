/* reject: all */
/* message: applied to inappropriate type */
/* A mode says a size of a kind of thing and not which kind, so the kind is the written type's.
   `SF` is a floating mode and an `unsigned int` is not a floating type, and there is no reading
   of the two together: taking the mode would throw the written type away and taking the type
   would throw the mode away. gcc refuses it in the same words. */

typedef unsigned int __attribute__((mode(SF))) confused;

confused f(confused a) { return a; }
