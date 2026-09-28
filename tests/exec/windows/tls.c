/* _Thread_local variables in four threads. Each thread changes its own copy, so each sees exactly
   what it did and the main thread's copies are untouched. */
#include <stdio.h>
#include <windows.h>

_Thread_local int counter;
_Thread_local int start = 7;
static _Thread_local int table[4] = {1, 2, 3, 4};
_Thread_local struct { char tag; long long total; } pair = {'p', 100};
static int seen[4];
static int sums[4];
static long long totals[4];

static DWORD WINAPI work(LPVOID arg) {
    int id = (int)(INT_PTR)arg;
    for (int i = 0; i < 1000 * (id + 1); i++)
        counter++;
    seen[id] = counter + start;
    table[id] += id * 10;
    int sum = 0;
    for (int i = 0; i < 4; i++)
        sum += table[i];
    sums[id] = sum;
    int *p = &pair.tag == 0 ? 0 : &table[id];
    pair.total += *p;
    totals[id] = pair.total;
    return 0;
}

int main(void) {
    HANDLE threads[4];
    for (int i = 0; i < 4; i++)
        threads[i] = CreateThread(NULL, 0, work, (LPVOID)(INT_PTR)i, 0, NULL);
    WaitForMultipleObjects(4, threads, TRUE, INFINITE);
    for (int i = 0; i < 4; i++)
        printf("thread %d saw %d %d %lld\n", i, seen[i], sums[i], totals[i]);
    printf("main saw %d %d %d %c %lld\n", counter, start, table[3], pair.tag, pair.total);
    return 0;
}
