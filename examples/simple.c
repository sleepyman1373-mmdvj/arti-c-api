/* simple.c - Minimal example for arti-c.
 *
 * Starts Arti with a per-run data directory, waits until it bootstraps,
 * then fetches http://example.com/ through the built-in SOCKS5 proxy
 * (127.0.0.1:<port>) and prints the response.
 *
 * Build against the shared library (see CMakeLists.txt), or compile the
 * same code for dynamic loading via dlopen()/LoadLibrary(): the example
 * only uses the functions declared in arti.h.
 *
 * Usage: simple [data_dir] [socks_port]
 */

#include "arti.h"

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
typedef SOCKET sock_t;
#define CLOSE_SOCK(s) closesocket(s)
#define SOCK_ERR WSAEWOULDBLOCK
#else
#include <netdb.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>
typedef int sock_t;
#define CLOSE_SOCK(s) close(s)
#define INVALID_SOCKET (-1)
#define SOCK_ERR EINPROGRESS
#define SOCKET_ERROR (-1)
#endif

/* Connect (plain TCP) to 127.0.0.1:port. */
static sock_t connect_socks(uint16_t port)
{
    struct sockaddr_in sa;
    sock_t s;

#ifdef _WIN32
    WSADATA wsa;
    if (WSAStartup(MAKEWORD(2, 2), &wsa) != 0) {
        fprintf(stderr, "WSAStartup failed\n");
        return INVALID_SOCKET;
    }
#endif

    s = socket(AF_INET, SOCK_STREAM, 0);
    if (s == INVALID_SOCKET)
        return s;

    memset(&sa, 0, sizeof(sa));
    sa.sin_family = AF_INET;
    sa.sin_port = htons(port);
    sa.sin_addr.s_addr = htonl(INADDR_LOOPBACK);

    if (connect(s, (struct sockaddr *)&sa, sizeof(sa)) == SOCKET_ERROR) {
        CLOSE_SOCK(s);
        return INVALID_SOCKET;
    }
    return s;
}

/* Perform a minimal SOCKS5 CONNECT handshake to host:port on sock. */
static int socks5_connect(sock_t s, const char *host, uint16_t port)
{
    unsigned char req[512];
    size_t req_len = 0;
    unsigned char resp[8];
    size_t hlen = strlen(host);

    if (hlen > 255)
        return -1;

    req[req_len++] = 0x05; /* version */
    req[req_len++] = 0x01; /* one method: no auth */
    req[req_len++] = 0x00;
    req[req_len++] = 0x05; /* version */
    req[req_len++] = 0x01; /* CONNECT */
    req[req_len++] = 0x00; /* reserved */
    req[req_len++] = 0x03; /* address type: hostname */
    req[req_len++] = (unsigned char)hlen;
    memcpy(req + req_len, host, hlen);
    req_len += hlen;
    req[req_len++] = (unsigned char)(port >> 8);
    req[req_len++] = (unsigned char)(port & 0xff);

    if (send(s, (const char *)req, (int)req_len, 0) != (int)req_len)
        return -1;
    if (recv(s, (char *)resp, sizeof(resp), 0) < 2 || resp[0] != 0x05 ||
        resp[1] != 0x00)
        return -1;
    return 0;
}

int main(int argc, char **argv)
{
    const char *data_dir = (argc > 1) ? argv[1] : NULL;
    uint16_t port_arg = (argc > 2) ? (uint16_t)atoi(argv[2]) : 0;
    arti *client;
    const char *ver;
    uint16_t port;
    int attempts;
    sock_t sock;
    const char *request;
    char buf[4096];
    int n;

    ver = arti_version();
    printf("%s\n", ver);

    client = arti_start(data_dir, port_arg);
    if (client == NULL) {
        const char *err = arti_last_error(NULL);
        fprintf(stderr, "arti_start failed: %s\n", err ? err : "unknown error");
        return 1;
    }

    port = arti_socks_port(client);
    printf("SOCKS5 proxy listening on 127.0.0.1:%u\n", (unsigned)port);
    printf("bootstrapping onto the Tor network (this can take a minute)...\n");

    for (attempts = 0; attempts < 120; attempts++) {
        int ready = arti_is_ready(client);
        if (ready == 1)
            break;
        if (ready < 0) {
            const char *err = arti_last_error(client);
            fprintf(stderr, "bootstrap failed: %s\n", err ? err : "unknown error");
            arti_stop(client);
            return 1;
        }
#ifdef _WIN32
        Sleep(1000);
#else
        sleep(1);
#endif
    }
    if (!arti_is_ready(client)) {
        fprintf(stderr, "timed out waiting for bootstrap\n");
        arti_stop(client);
        return 1;
    }
    printf("bootstrapped!\n");

    sock = connect_socks(port);
    if (sock == INVALID_SOCKET) {
        fprintf(stderr, "could not connect to local SOCKS proxy\n");
        arti_stop(client);
        return 1;
    }
    if (socks5_connect(sock, "example.com", 80) != 0) {
        fprintf(stderr, "SOCKS5 handshake failed\n");
        CLOSE_SOCK(sock);
        arti_stop(client);
        return 1;
    }

    request = "GET / HTTP/1.1\r\nHost: example.com\r\n"
              "Connection: close\r\n\r\n";
    send(sock, request, (int)strlen(request), 0);
    while ((n = (int)recv(sock, buf, sizeof(buf), 0)) > 0)
        fwrite(buf, 1, (size_t)n, stdout);
    printf("\n");

    CLOSE_SOCK(sock);
#ifdef _WIN32
    WSACleanup();
#endif
    arti_stop(client);
    return 0;
}
