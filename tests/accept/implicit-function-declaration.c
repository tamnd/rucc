/* accept: c89 gnu89 */
/* reject: c99 c11 c17 c23 gnu99 gnu11 gnu17 gnu23 */
/* message: implicit declaration of function 'unknown' */
/* Calling a function nobody declared was C89's way of saying it returns `int`. C99 removed it
   and gcc has made it an error rather than a warning. Under C89 it is the language and gcc says
   nothing about it without `-Wall`, which is why there is no `warns` line here. */

int use(void) {
  return unknown(1);
}
