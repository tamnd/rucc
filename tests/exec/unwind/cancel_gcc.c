#include <pthread.h>
#include <semaphore.h>
#include <stdio.h>
#include <unistd.h>

void *deferred(void *arg);
void *async(void *arg);

static sem_t ready;

void started(void)
{
    sem_post(&ready);
}

static void one(const char *name, void *(*fn)(void *))
{
    pthread_t t;
    void *res;
    sem_init(&ready, 0, 0);
    pthread_create(&t, 0, fn, 0);
    sem_wait(&ready);
    usleep(50000);
    pthread_cancel(t);
    pthread_join(t, &res);
    printf("%s %s\n", name, res == PTHREAD_CANCELED ? "canceled" : "returned");
}

int main(void)
{
    one("deferred", deferred);
    one("async", async);
    return 0;
}
