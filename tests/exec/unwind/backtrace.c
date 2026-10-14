/* backtrace() in the other half, called from three frames of this half. glibc walks the stack with
 * _Unwind_Backtrace, so a wrong CFA rule in this half gives a short or a wrong list. None of the
 * calls is a tail call, so each frame is on the stack. */
int bottom(int k);

__attribute__((noinline)) int third(int k)
{
    return bottom(k) + 1;
}

__attribute__((noinline)) int second(int k)
{
    return third(k + 1) * 2;
}

__attribute__((noinline)) int first(int k)
{
    return second(k + 1) - 3;
}
