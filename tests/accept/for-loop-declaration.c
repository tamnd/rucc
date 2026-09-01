/* accept: c99 c11 c17 c23 gnu99 gnu11 gnu17 gnu23 */
/* reject: c89 gnu89 */
/* gap: #98 c89 gnu89 */
/* Declaring the loop variable in the `for` is C99. gcc refuses it in gnu89 too, which is one of
   the places the gnu dialects are not simply the iso ones with more allowed. */

int sum(void) {
  int total = 0;
  for (int i = 0; i < 4; i++) {
    total += i;
  }
  return total;
}
