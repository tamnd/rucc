#include <cstdio>
#include <stdexcept>

extern "C" int twice(int *v, int n);

extern "C" void check(int a, int b)
{
    if (a == 7 || b == 7)
        throw std::runtime_error("seven");
}

int main()
{
    int a[] = {3, 1, 2};
    std::printf("twice %d\n", twice(a, 3));
    int b[] = {9, 7, 5, 3};
    try {
        twice(b, 4);
        std::puts("not thrown");
    } catch (const std::exception &e) {
        std::printf("caught %s\n", e.what());
    }
    return 0;
}
