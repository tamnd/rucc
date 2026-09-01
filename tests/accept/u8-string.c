/* accept: c11 c17 c23 gnu99 gnu11 gnu17 gnu23 */
/* reject: c89 c99 gnu89 */
/* gap: #99 c11 c17 c23 gnu99 gnu11 gnu17 gnu23 */
/* A `u8` string is C11, and gcc has it in the gnu dialects from gnu99 on. */

const char *greeting = u8"hello";
