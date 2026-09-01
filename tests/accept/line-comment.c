/* accept: c99 c11 c17 c23 gnu */
/* reject: c89 */
/* gap: #98 c89 */
/* `//` is not a comment in C89, so the line below is a syntax error there and a declaration
   everywhere else. gcc accepts it in gnu89 as an extension, and this file is written the way
   gcc reads it. The directives above are the old kind of comment for the same reason: this
   file has to be readable by the dialect it is about. */

// a comment that is only a comment in some of these
int declared_after_a_line_comment;
