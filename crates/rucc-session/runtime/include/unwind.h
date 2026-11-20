/* unwind.h, the interface of the unwinder that libgcc_s carries.
 *
 * gcc ships this header and a program includes it to walk its own stack with `_Unwind_Backtrace`,
 * as the x86 selftest `unwind_vdso` does to check the unwind information of the vDSO. The code
 * behind it is libgcc's, which the link already asks for, so this is the declarations alone, laid
 * out as the Itanium C++ ABI and gcc's header lay them out.
 *
 * ARM's 32 bit EHABI has an unwinder of another shape, with `_Unwind_GetGR` written over
 * `_Unwind_VRS_Get`, and is not described here.
 */

#ifndef __RUCC_UNWIND_H
#define __RUCC_UNWIND_H

#if defined(__arm__) && !defined(__aarch64__)
#error "unwind.h: the ARM EHABI unwinder is not described by this header"
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef unsigned _Unwind_Word __attribute__((__mode__(__unwind_word__)));
typedef signed _Unwind_Sword __attribute__((__mode__(__unwind_word__)));
typedef unsigned _Unwind_Ptr __attribute__((__mode__(__pointer__)));
typedef unsigned _Unwind_Internal_Ptr __attribute__((__mode__(__pointer__)));
typedef unsigned _Unwind_Exception_Class __attribute__((__mode__(__DI__)));

typedef enum {
  _URC_NO_REASON = 0,
  _URC_FOREIGN_EXCEPTION_CAUGHT = 1,
  _URC_FATAL_PHASE2_ERROR = 2,
  _URC_FATAL_PHASE1_ERROR = 3,
  _URC_NORMAL_STOP = 4,
  _URC_END_OF_STACK = 5,
  _URC_HANDLER_FOUND = 6,
  _URC_INSTALL_CONTEXT = 7,
  _URC_CONTINUE_UNWIND = 8
} _Unwind_Reason_Code;

typedef int _Unwind_Action;

#define _UA_SEARCH_PHASE 1
#define _UA_CLEANUP_PHASE 2
#define _UA_HANDLER_FRAME 4
#define _UA_FORCE_UNWIND 8
#define _UA_END_OF_STACK 16

struct _Unwind_Exception;

typedef void (*_Unwind_Exception_Cleanup_Fn)(_Unwind_Reason_Code, struct _Unwind_Exception *);

struct _Unwind_Exception {
  _Unwind_Exception_Class exception_class;
  _Unwind_Exception_Cleanup_Fn exception_cleanup;
  _Unwind_Word private_1;
  _Unwind_Word private_2;
} __attribute__((__aligned__));

struct _Unwind_Context;

typedef _Unwind_Reason_Code (*_Unwind_Stop_Fn)(int, _Unwind_Action, _Unwind_Exception_Class,
                                               struct _Unwind_Exception *,
                                               struct _Unwind_Context *, void *);
typedef _Unwind_Reason_Code (*_Unwind_Trace_Fn)(struct _Unwind_Context *, void *);
typedef _Unwind_Reason_Code (*_Unwind_Personality_Fn)(int, _Unwind_Action,
                                                      _Unwind_Exception_Class,
                                                      struct _Unwind_Exception *,
                                                      struct _Unwind_Context *);

_Unwind_Reason_Code _Unwind_RaiseException(struct _Unwind_Exception *);
_Unwind_Reason_Code _Unwind_ForcedUnwind(struct _Unwind_Exception *, _Unwind_Stop_Fn, void *);
void _Unwind_DeleteException(struct _Unwind_Exception *);
void _Unwind_Resume(struct _Unwind_Exception *);
_Unwind_Reason_Code _Unwind_Resume_or_Rethrow(struct _Unwind_Exception *);
_Unwind_Reason_Code _Unwind_Backtrace(_Unwind_Trace_Fn, void *);

_Unwind_Word _Unwind_GetGR(struct _Unwind_Context *, int);
void _Unwind_SetGR(struct _Unwind_Context *, int, _Unwind_Word);
_Unwind_Ptr _Unwind_GetIP(struct _Unwind_Context *);
_Unwind_Ptr _Unwind_GetIPInfo(struct _Unwind_Context *, int *);
void _Unwind_SetIP(struct _Unwind_Context *, _Unwind_Ptr);
_Unwind_Word _Unwind_GetCFA(struct _Unwind_Context *);
void *_Unwind_GetLanguageSpecificData(struct _Unwind_Context *);
_Unwind_Ptr _Unwind_GetRegionStart(struct _Unwind_Context *);
void *_Unwind_FindEnclosingFunction(void *);
_Unwind_Ptr _Unwind_GetDataRelBase(struct _Unwind_Context *);
_Unwind_Ptr _Unwind_GetTextRelBase(struct _Unwind_Context *);

#ifdef __cplusplus
}
#endif

#endif
