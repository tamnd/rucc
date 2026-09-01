/* accept: c23 gnu23 */
/* reject: c89 c99 c11 c17 gnu89 gnu99 gnu11 gnu17 */
/* gap: #101 c23 gnu23 */
/* Being usable where a constant is required is the whole reason the keyword exists, and it is
   the part rucc does not do yet. */

constexpr int side = 4;
int square[side * side];
