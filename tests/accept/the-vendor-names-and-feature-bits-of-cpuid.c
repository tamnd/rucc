/* accept: all */
/* <cpuid.h> gives the vendor names leaf 0 answers and the feature bits as the header of gcc 16
   does. NetBSD's aes_via.c compares the vendor names to tell a VIA processor. */

#include <cpuid.h>

int via(void)
{
  unsigned int eax, ebx, ecx, edx;
  __cpuid(0, eax, ebx, ecx, edx);
  return eax != 0 && ebx == signature_CENTAUR_ebx && ecx == signature_CENTAUR_ecx
         && edx == signature_CENTAUR_edx;
}

unsigned int bits[] = { bit_3DNOW, bit_XOP, bit_SGX, bit_SHSTK, bit_AVX512FP16, bit_SHA512,
                        bit_APX_F, bit_PTWRITE, bit_AMX_FP8, signature_SHANGHAI_edx };
