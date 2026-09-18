/* A workload against an instrumented SQLite, with answers it has to get right. */
/* The point of this program is that it is not a memory safety test. It builds a table, fills it,
   indexes it, and asks two questions whose answers are arithmetic rather than opinion, so a build
   that gets them right has run a real library end to end under the monitor. Every case in
   tests/safety is a few lines of C written to provoke one judgement, and a library that recycles
   its own memory through a lookaside allocator and hands the same block out as three different
   structures is a thing none of them look like. tamnd/rucc#1307 is what happens when nothing here
   does.

   The declarations are written out rather than included because rucc has no built-in system
   include directories and nothing here should depend on which sqlite3.h the machine happens to
   have. Four prototypes are the same four prototypes everywhere.

   The answers: 4000 rows numbered 0 to 3999, and the count is of those whose text begins "row 1",
   which is row 1, rows 10 to 19, rows 100 to 199 and rows 1000 to 1999, so 1111 of them. Their
   numbers sum to 1514596. The ordering query is by text rather than by number, so the first row
   back is "row 0" and not "row 1", which is the cheapest way to tell an index walk that worked
   from one that returned the table. None of those depend on the version of SQLite this is built
   against. */
#include <stdio.h>
#include <string.h>

typedef struct sqlite3 sqlite3;
int sqlite3_open(const char *, sqlite3 **);
int sqlite3_exec(sqlite3 *, const char *, int (*)(void *, int, char **, char **), void *, char **);
int sqlite3_close(sqlite3 *);

static long count_seen;
static long sum_seen;
static long rows_seen;
static char first_b[64];

static int grab(void *unused, int n, char **vals, char **names) {
    (void)unused;
    (void)names;
    if (n == 2 && vals[0] && vals[1] && count_seen == 0) {
        sscanf(vals[0], "%ld", &count_seen);
        sscanf(vals[1], "%ld", &sum_seen);
    }
    return 0;
}

static int rowcb(void *unused, int n, char **vals, char **names) {
    (void)unused;
    (void)n;
    (void)names;
    if (rows_seen == 0 && vals[1]) {
        strncpy(first_b, vals[1], sizeof first_b - 1);
    }
    rows_seen++;
    return 0;
}

int main(void) {
    sqlite3 *db;
    char sql[128];
    int i;
    int bad = 0;
    if (sqlite3_open(":memory:", &db)) return 1;
    if (sqlite3_exec(db, "create table t(a integer, b text);", 0, 0, 0)) return 2;
    sqlite3_exec(db, "begin;", 0, 0, 0);
    for (i = 0; i < 4000; i++) {
        snprintf(sql, sizeof sql, "insert into t values(%d, 'row %d');", i, i);
        if (sqlite3_exec(db, sql, 0, 0, 0)) return 3;
    }
    if (sqlite3_exec(db, "commit;", 0, 0, 0)) return 4;
    if (sqlite3_exec(db, "create index ti on t(b);", 0, 0, 0)) return 5;
    if (sqlite3_exec(db, "select count(*), sum(a) from t where b like 'row 1%';", grab, 0, 0)) {
        return 6;
    }
    if (sqlite3_exec(db, "select a, b from t order by b limit 50;", rowcb, 0, 0)) return 7;
    sqlite3_close(db);

    printf("count=%ld sum=%ld rows=%ld first=%s\n", count_seen, sum_seen, rows_seen, first_b);
    if (count_seen != 1111) bad = 1;
    if (sum_seen != 1514596) bad = 1;
    if (rows_seen != 50) bad = 1;
    if (strcmp(first_b, "row 0") != 0) bad = 1;
    printf("%s\n", bad ? "MISMATCH" : "all answers correct");
    return bad;
}
