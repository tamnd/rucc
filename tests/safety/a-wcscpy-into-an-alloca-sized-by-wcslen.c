/* row: S8 */
/* allow */
typedef int wchar_t;
unsigned long wcslen(const wchar_t *s);
wchar_t *wcscpy(wchar_t *to, const wchar_t *from);
wchar_t *wcscat(wchar_t *to, const wchar_t *from);
/* The good half of the row above. Measured with wcslen the buffer has room for every character
   and the terminator, and a wrapper that judged the count as bytes rather than characters would
   refuse this. */
int main(void) {
    wchar_t *data = L"AAAAAAAAAA";
    unsigned long len = wcslen(data);
    wchar_t *dest = __builtin_alloca((2 * len + 1) * sizeof(wchar_t));
    wcscpy(dest, data);
    wcscat(dest, data);
    return wcslen(dest) == 2 * len ? 0 : 1;
}
