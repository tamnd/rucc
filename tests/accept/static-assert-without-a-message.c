/* accept: c23 gnu23 */
/* reject: c89 c99 c11 c17 gnu89 gnu99 gnu11 gnu17 */
/* C11 has `_Static_assert(expression, message)` and C23 adds the spelling `static_assert` and
   makes the message optional. This file uses both of the C23 parts at once. */

static_assert(1 + 1 == 2);
