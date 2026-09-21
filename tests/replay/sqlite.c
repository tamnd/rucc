/* The SQLite harness, which is SQLite's own test/ossfuzz.c written out in this repository's C. */
/* Upstream is `test/ossfuzz.c` in the SQLite tree, which the amalgamation tarball this repository
   builds SQLite from does not carry, so it is written here rather than taken. The changes are the
   ones the surrounding code needs and no others: the spacing and brace style of the C here, the
   two `_MSC_VER` blocks dropped because a replay only runs on Linux, and the fixed width types in
   the entry point spelled the way the driver declares them. Everything the harness actually does
   is upstream's, because this is one of the few targets whose harness is written by the library's
   own author and every limit in it was put there for a reason somebody hit.

   The input is SQL, with a twist that matters for reading a corpus. If the second byte is a
   newline then the first byte is a selector and the SQL starts at the third: bit zero of the
   selector turns on foreign key constraints and the remaining bits cap how many result rows the
   exec callback will take before it asks to stop. If the second byte is not a newline then the
   whole input is SQL and the selector is 0xfd. So an input in this corpus is usually a two byte
   header and a query, and a harness that treated the whole thing as SQL would run a different
   program than the one those inputs were selected against.

   The limits are worth knowing because they are why this target does not need a clock from us.
   A progress handler runs every ten virtual machine steps and stops the statement once ten seconds
   of wall clock have gone by, there is a cap of 25,000 opcodes on a prepared statement, a 20 MB
   hard heap limit, a 50,000 byte cap on any string or blob so that `randomblob(N)` cannot be asked
   for a gigabyte, and a 250 byte cap on a LIKE or GLOB pattern that upstream added after a real
   timeout report. The authorizer refuses the debugging pragmas, which produce enormous output and
   test nothing. The database is opened in memory, so a replay of forty thousand inputs writes no
   files.

   One thing the ten second cutoff cannot help with is that the cutoff is wall clock and this build
   is instrumented, so a statement that upstream would have let run to completion can be stopped
   part way here. That costs coverage rather than correctness, and it is the same direction as
   every other slowdown in this task. */
#include <stdio.h>
#include <string.h>

#include "sqlite3.h"

/* Upstream's debugging flags, which OSS-Fuzz leaves off and so does this. They are kept because the
   branches that read them are code, and because `ossfuzz_set_debug_flags` is how upstream's own
   `ossshell` utility drives this same file. */
static unsigned mDebug = 0;
#define FUZZ_SQL_TRACE 0x0001
#define FUZZ_SHOW_MAX_DELAY 0x0002
#define FUZZ_SHOW_ERRORS 0x0004

void ossfuzz_set_debug_flags(unsigned x) {
    mDebug = x;
}

/* The time of day in milliseconds, taken through whichever VFS is first, because that is the clock
   the library itself would use. */
static sqlite3_int64 timeOfDay(void) {
    static sqlite3_vfs *clockVfs = 0;
    sqlite3_int64 t;

    if (clockVfs == 0) {
        clockVfs = sqlite3_vfs_find(0);
        if (clockVfs == 0) {
            return 0;
        }
    }
    if (clockVfs->iVersion >= 2 && clockVfs->xCurrentTimeInt64 != 0) {
        clockVfs->xCurrentTimeInt64(clockVfs, &t);
    } else {
        double r;

        clockVfs->xCurrentTime(clockVfs, &r);
        t = (sqlite3_int64)(r * 86400000.0);
    }
    return t;
}

/* What the callbacks are handed a pointer to. */
typedef struct FuzzCtx {
    sqlite3 *db;               /* The database connection. */
    sqlite3_int64 iCutoffTime; /* Stop once the clock passes this. */
    sqlite3_int64 iLastCb;     /* When the previous progress callback ran. */
    sqlite3_int64 mxInterval;  /* The longest gap between two of them. */
    unsigned nCb;              /* How many there have been. */
    unsigned execCnt;          /* How many more rows the exec callback will take. */
} FuzzCtx;

/* Returns non zero once the cutoff has passed, which is how a statement is stopped. */
static int progress_handler(void *pClientData) {
    FuzzCtx *p = (FuzzCtx *)pClientData;
    sqlite3_int64 iNow = timeOfDay();
    int rc = iNow >= p->iCutoffTime;
    sqlite3_int64 iDiff = iNow - p->iLastCb;

    if (iDiff > p->mxInterval) {
        p->mxInterval = iDiff;
    }
    p->nCb++;
    return rc;
}

/* Refuses `PRAGMA vdbe_*` and `PRAGMA parser_trace`, which print a great deal and prove nothing. */
static int block_debug_pragmas(void *Notused, int eCode, const char *zArg1, const char *zArg2,
                               const char *zArg3, const char *zArg4) {
    (void)Notused;
    (void)zArg2;
    (void)zArg3;
    (void)zArg4;
    if (eCode == SQLITE_PRAGMA
        && (sqlite3_strnicmp("vdbe_", zArg1, 5) == 0
            || sqlite3_stricmp("parser_trace", zArg1) == 0)) {
        return SQLITE_DENY;
    }
    return SQLITE_OK;
}

/* Formats every column of every row and throws it away, which is what makes the query actually run,
   and asks to stop once the row budget or the clock is gone. */
static int exec_handler(void *pClientData, int argc, char **argv, char **namev) {
    FuzzCtx *p = (FuzzCtx *)pClientData;
    int i;

    (void)namev;
    if (argv) {
        for (i = 0; i < argc; i++) {
            sqlite3_free(sqlite3_mprintf("%s", argv[i]));
        }
    }
    return (p->execCnt--) <= 0 || progress_handler(pClientData);
}

int LLVMFuzzerTestOneInput(const unsigned char *data, unsigned long size) {
    char *zErrMsg = 0;
    unsigned char uSelector;
    int rc;
    char *zSql;
    FuzzCtx cx;

    memset(&cx, 0, sizeof(cx));
    if (size < 3) {
        return 0;
    }

    /* The selector is the first byte, but only when the second one is a newline. */
    if (data[1] == '\n') {
        uSelector = data[0];
        data += 2;
        size -= 2;
    } else {
        uSelector = 0xfd;
    }

    /* In memory only, so a replay leaves nothing behind. */
    if (sqlite3_initialize()) {
        return 0;
    }
    rc = sqlite3_open_v2("fuzz.db", &cx.db,
                         SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_MEMORY, 0);
    if (rc) {
        return 0;
    }

    /* Ask often enough that a runaway statement is stopped within ten seconds of starting. */
    cx.iLastCb = timeOfDay();
    cx.iCutoffTime = cx.iLastCb + 10000;
#ifndef SQLITE_OMIT_PROGRESS_CALLBACK
    sqlite3_progress_handler(cx.db, 10, progress_handler, (void *)&cx);
#endif

    sqlite3_limit(cx.db, SQLITE_LIMIT_VDBE_OP, 25000);
    sqlite3_limit(cx.db, SQLITE_LIMIT_LIKE_PATTERN_LENGTH, 250);
    sqlite3_hard_heap_limit64(20000000);
    sqlite3_limit(cx.db, SQLITE_LIMIT_LENGTH, 50000);

    /* Bit zero of the selector turns on foreign keys, and what is left of it is the row budget. */
    sqlite3_db_config(cx.db, SQLITE_DBCONFIG_ENABLE_FKEY, uSelector & 1, &rc);
    uSelector >>= 1;
    sqlite3_set_authorizer(cx.db, block_debug_pragmas, 0);
    cx.execCnt = uSelector + 1;

    /* `sqlite3_exec` wants a zero terminated string, so the input is copied into one. */
    zSql = sqlite3_mprintf("%.*s", (int)size, data);
#ifndef SQLITE_OMIT_COMPLETE
    sqlite3_complete(zSql);
#endif
    sqlite3_exec(cx.db, zSql, exec_handler, (void *)&cx, &zErrMsg);

    if ((mDebug & FUZZ_SHOW_ERRORS) != 0 && zErrMsg) {
        printf("Error: %s\n", zErrMsg);
    }

    sqlite3_free(zErrMsg);
    sqlite3_free(zSql);
    sqlite3_exec(cx.db, "PRAGMA temp_store_directory=''", 0, 0, 0);
    sqlite3_close(cx.db);

    if (mDebug & FUZZ_SHOW_MAX_DELAY) {
        printf("Progress callback count....... %d\n", cx.nCb);
        printf("Max time between callbacks.... %d ms\n", (int)cx.mxInterval);
    }
    return 0;
}
