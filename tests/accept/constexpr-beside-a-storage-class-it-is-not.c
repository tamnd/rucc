/* reject: all */
/* The other half of C23 6.7.1: the three pairings it does not name are still two storage class
   specifiers, and this is one of them. gcc names both keywords rather than saying that there
   are two of them, and says them in the same order however the pair was written, which is what
   the parser's own tests check. Before C23 the line is refused for a duller reason, since the
   keyword is an ordinary identifier there and this is a declaration with two of them. */

constexpr extern int x = 1;
