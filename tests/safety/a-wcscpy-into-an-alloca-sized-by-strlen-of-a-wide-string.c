/* row: S8 */
/* refuse: J1 */
/* says: in wcscpy, over its dst argument */
typedef int wchar_t;
unsigned long strlen(const char *s);
wchar_t *wcscpy(wchar_t *to, const wchar_t *from);
/* Juliet's CWE135. A wide string measured with strlen stops at the first zero byte, which is the
   second byte of its first character, so the buffer has room for two characters and the copy
   writes eleven. The counts of the wide functions are in characters, and the wrapper judges the
   destination for every byte the copy reaches. */
int main(void) {
    wchar_t *data = L"AAAAAAAAAA";
    unsigned long len = strlen((char *)data);
    wchar_t *dest = __builtin_alloca((len + 1) * sizeof(wchar_t));
    wcscpy(dest, data);
    return dest[0] == 'A' ? 0 : 1;
}
