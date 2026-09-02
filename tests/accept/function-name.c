/* accept: all */
/* `__func__` arrived in C99 and gcc accepts it in C89 too, warning only under `-pedantic`, so
   every dialect takes it. `__FUNCTION__` and `__PRETTY_FUNCTION__` are GNU's spellings of the
   same thing, which in C say what `__func__` says and are accepted everywhere as well. */

const char *standard(void)
{
    return __func__;
}

const char *gnu(void)
{
    return __FUNCTION__;
}

const char *pretty(void)
{
    return __PRETTY_FUNCTION__;
}

unsigned long length(void)
{
    return sizeof __func__;
}
