#include <cstdio>
#include <pthread.h>

extern "C" int outer(int k);

extern "C" void deep(int k)
{
    std::printf("deep %d\n", k);
    std::fflush(stdout);
    if (k == 1)
        throw 1;
    if (k == 2)
        pthread_exit(nullptr);
}

static void *run(void *)
{
    outer(2);
    return nullptr;
}

int main()
{
    std::printf("returned %d\n", outer(0));
    try {
        outer(1);
        std::puts("not thrown");
    } catch (int k) {
        std::printf("caught %d\n", k);
    }
    pthread_t t;
    void *res;
    pthread_create(&t, nullptr, run, nullptr);
    pthread_join(t, &res);
    std::printf("thread %s\n", res == PTHREAD_CANCELED ? "canceled" : "exited");
    return 0;
}
