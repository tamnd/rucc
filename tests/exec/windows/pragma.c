/* gcc flags: -lws2_32 */
// A program that asks for ws2_32 with #pragma comment and not on the command line, which only
// links if the pragma reached the linker through .drectve. gcc ignores the pragma, so pragma.out
// was written by hand from what the program prints when it links.
#include <stdio.h>
#include <winsock2.h>

#pragma comment(lib, "ws2_32")
#pragma comment(user, "nothing reads this")

int main(void) {
    WSADATA data;
    int started = WSAStartup(MAKEWORD(2, 2), &data);
    printf("started %d version %d.%d\n", started, LOBYTE(data.wVersion), HIBYTE(data.wVersion));
    u_short port = htons(0x1234);
    printf("htons %x\n", port);
    printf("cleanup %d\n", WSACleanup());
    return 0;
}
