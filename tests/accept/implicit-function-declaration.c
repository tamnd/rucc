/* accept: c89 gnu89 */
/* reject: c99 c11 c17 c23 gnu99 gnu11 gnu17 gnu23 */
/* gap: #84 c89 gnu89 */
/* Calling a function nobody declared was C89's way of saying it returns `int`. C99 removed it
   and gcc has made it an error rather than a warning. There is no `message` here because the
   wording is still the one for a name rather than the one for a call, which is the other half
   of issue #84. */

int use(void) {
  return unknown(1);
}
