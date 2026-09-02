/* accept: all */
/* No header declares a builtin, and the reserved prefix is what says the name belongs to the
   implementation, so the implementation declares it on first use. SQLite calls these directly
   under `__GNUC__`, which is the path every build that is not MSVC takes. */

double up(double x)
{
    return __builtin_ceil(x);
}

float up_narrow(float x)
{
    return __builtin_ceilf(x);
}

long double up_wide(long double x)
{
    return __builtin_ceill(x);
}

double down(double x)
{
    return __builtin_floor(x);
}

float down_narrow(float x)
{
    return __builtin_floorf(x);
}

long double down_wide(long double x)
{
    return __builtin_floorl(x);
}
