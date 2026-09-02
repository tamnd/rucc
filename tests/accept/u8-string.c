/* accept: c11 c17 c23 gnu99 gnu11 gnu17 gnu23 */
/* reject: c89 c99 gnu89 */
/* message: this encoding prefix is not available in this dialect */
/* A `u8` string is C11, and gcc has it in the gnu dialects from gnu99 on. */

int first(void) { return u8"hello"[0]; }

/* C23 gave the prefix the type `char8_t`, which in C is `unsigned char`. Before that the
   literal was an array of plain `char`, and gcc 16 answers a `_Generic` the same way. */
int element_type(void)
{
#if __STDC_VERSION__ >= 202311L
    return _Generic(u8"x", unsigned char *: 1);
#else
    return _Generic(u8"x", char *: 1);
#endif
}
