/* accept: c23 gnu23 */
/* reject: c89 c99 c11 c17 gnu89 gnu99 gnu11 gnu17 */
/* gap: #98 c89 c99 c11 c17 gnu89 gnu99 gnu11 gnu17 */
/* The `'` between digits is C23 and is not a GNU extension, so the gnu dialects before C23 do
   not have it either. */

int million = 1'000'000;
