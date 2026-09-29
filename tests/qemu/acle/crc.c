/*
 * The CRC32 and CRC32C steps from <arm_acle.h>, held against the same checksums worked out a bit
 * at a time in plain C.
 *
 * Each checksum is taken over a set of buffers the way a program that uses the intrinsics takes
 * it: a byte at a time until the pointer is eight byte aligned, then eight bytes at a time, then
 * a word, a half and a byte for the tail, so every one of the eight steps is used on every
 * buffer longer than a few bytes. The software reading is the reflected polynomial worked through
 * bit by bit, with nothing borrowed from the hardware, so the two agreeing says the instructions
 * were encoded and given their operands the right way round.
 *
 * The functions that use the steps carry target("+crc"), so this builds without a flag as well as
 * under -march=armv8-a+crc, and both are run. Everything printed is a value, never an address,
 * so gcc's build of this file is the reference for rucc's.
 */

#include <arm_acle.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static uint32_t soft(uint32_t crc, const unsigned char *p, size_t n, uint32_t poly)
{
	while (n--) {
		crc ^= *p++;
		for (int bit = 0; bit < 8; bit++)
			crc = crc & 1 ? (crc >> 1) ^ poly : crc >> 1;
	}
	return crc;
}

__attribute__((target("+crc"))) static uint32_t hard(uint32_t crc, const unsigned char *p,
						     size_t n, int castagnoli)
{
	while (n && ((uintptr_t)p & 7)) {
		crc = castagnoli ? __crc32cb(crc, *p) : __crc32b(crc, *p);
		p++;
		n--;
	}
	while (n >= 8) {
		uint64_t d;
		memcpy(&d, p, 8);
		crc = castagnoli ? __crc32cd(crc, d) : __crc32d(crc, d);
		p += 8;
		n -= 8;
	}
	if (n >= 4) {
		uint32_t w;
		memcpy(&w, p, 4);
		crc = castagnoli ? __crc32cw(crc, w) : __crc32w(crc, w);
		p += 4;
		n -= 4;
	}
	if (n >= 2) {
		uint16_t h;
		memcpy(&h, p, 2);
		crc = castagnoli ? __crc32ch(crc, h) : __crc32h(crc, h);
		p += 2;
		n -= 2;
	}
	if (n)
		crc = castagnoli ? __crc32cb(crc, *p) : __crc32b(crc, *p);
	return crc;
}

int main(void)
{
	static unsigned char buffer[300];
	int wrong = 0;

	for (size_t i = 0; i < sizeof buffer; i++)
		buffer[i] = (unsigned char)(i * 131 + 7);

	/* The check values every CRC catalogue gives for "123456789". */
	const unsigned char *check = (const unsigned char *)"123456789";
	uint32_t crc32 = ~hard(~0u, check, 9, 0);
	uint32_t crc32c = ~hard(~0u, check, 9, 1);
	printf("check crc32 %08x crc32c %08x\n", (unsigned)crc32, (unsigned)crc32c);
	wrong += crc32 != 0xcbf43926u;
	wrong += crc32c != 0xe3069283u;

	/* Every start from 0 to 7 and a spread of lengths, so each alignment reaches each tail. */
	static const size_t lengths[] = { 0, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 64, 255, 291 };
	for (size_t start = 0; start < 8; start++) {
		for (size_t k = 0; k < sizeof lengths / sizeof lengths[0]; k++) {
			size_t n = lengths[k];
			const unsigned char *p = buffer + start;
			uint32_t a = hard(0x12345678u, p, n, 0);
			uint32_t b = soft(0x12345678u, p, n, 0xedb88320u);
			uint32_t c = hard(0x9abcdef0u, p, n, 1);
			uint32_t d = soft(0x9abcdef0u, p, n, 0x82f63b78u);
			printf("start %zu length %3zu crc32 %08x crc32c %08x\n", start, n, (unsigned)a,
			       (unsigned)c);
			if (a != b || c != d) {
				printf("  the software reading says crc32 %08x crc32c %08x\n",
				       (unsigned)b, (unsigned)d);
				wrong++;
			}
		}
	}
	return wrong != 0;
}
