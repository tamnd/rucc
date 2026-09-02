/* accept: all */
/* A `#` and a number is the marker `-E` writes, and a preprocessed file handed back to the
   compiler is full of them. Zero is a line number here where `#line 0` is an error, the flags
   after the name are the nesting, and none of it is macro expanded. */

# 200 "generated.c"
int after_a_rename[__LINE__ == 200 ? 1 : -1];

# 0 "counted-from-zero.c"
int from_zero[__LINE__ == 0 ? 1 : -1];

# 10 "outer.c" 1
int entered[__LINE__ == 10 ? 1 : -1];

# 40 "counted-from-zero.c" 2
int returned[__LINE__ == 40 ? 1 : -1];

# 70 "system.c" 3 4
int flagged[__LINE__ == 70 ? 1 : -1];

# 90
int just_a_number[__LINE__ == 90 ? 1 : -1];
