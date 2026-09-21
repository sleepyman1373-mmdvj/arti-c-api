/* arti-proxy.cpp - Command-line SOCKS5 proxy front-end for arti-c.
 *
 * Starts Arti in-process and serves its SOCKS5 proxy on 127.0.0.1:<socks_port>
 * (default 9150, the same port `arti proxy` uses), then stays in the
 * foreground until the process is killed.
 *
 * This is a test harness: it deliberately does not wait for bootstrap and does
 * not install signal handlers, so it is ready to report its listening port
 * immediately while Arti is still connecting to the Tor network. Connections
 * accepted during that window are handled, but cannot be relayed until
 * bootstrap completes -- watch stderr, or run with RUST_LOG=info.
 *
 * Usage: arti-proxy [--socks-port N] [--data-dir DIR]
 */

#include "arti.h"

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>

#ifdef _WIN32
#include <windows.h>
#else
#include <unistd.h>
#endif

/* Matches the default of `arti proxy` (tor's socks_listen default port). */
static const uint16_t DEFAULT_SOCKS_PORT = 9150;

static void usage(const char *argv0)
{
    printf("usage: %s [--socks-port N] [--data-dir DIR]\n"
           "\n"
           "Serves the built-in SOCKS5 proxy on 127.0.0.1:<N> (default %u).\n"
           "Runs in the foreground; press Ctrl+C to exit.\n"
           "\n"
           "  --socks-port N   port to listen on; 0 selects arti-c's own\n"
           "                   default of 9050\n"
           "  --data-dir DIR   directory for persistent Tor state and cache\n"
           "                   (default: platform-default Arti storage)\n",
           argv0, (unsigned)DEFAULT_SOCKS_PORT);
}

/* Parse a decimal port into *out. Port 0 is accepted: arti_start() maps it to
 * its own default. Returns 0 on success. */
static int parse_port(const char *s, uint16_t *out)
{
    char *end = NULL;
    long v;

    if (s == NULL || *s == '\0')
        return -1;
    v = strtol(s, &end, 10);
    if (end == s || *end != '\0' || v < 0 || v > 65535)
        return -1;
    *out = (uint16_t)v;
    return 0;
}

int main(int argc, char **argv)
{
    uint16_t socks_port = DEFAULT_SOCKS_PORT;
    const char *data_dir = NULL;
    arti *client;

    for (int i = 1; i < argc; i++) {
        const char *arg = argv[i];

        if (strcmp(arg, "-h") == 0 || strcmp(arg, "--help") == 0) {
            usage(argv[0]);
            return 0;
        } else if (strcmp(arg, "--socks-port") == 0) {
            if (i + 1 >= argc || parse_port(argv[++i], &socks_port) != 0) {
                fprintf(stderr, "%s: --socks-port needs a port number (0-65535)\n",
                        argv[0]);
                return 2;
            }
        } else if (strcmp(arg, "--data-dir") == 0) {
            if (i + 1 >= argc) {
                fprintf(stderr, "%s: --data-dir needs an argument\n", argv[0]);
                return 2;
            }
            data_dir = argv[++i];
        } else {
            fprintf(stderr, "%s: unrecognized argument '%s'\n", argv[0], arg);
            usage(argv[0]);
            return 2;
        }
    }

    printf("%s\n", arti_version());

    client = arti_start(data_dir, socks_port);
    if (client == NULL) {
        /* Most likely cause: the port is already in use, or the data
         * directory cannot be created. */
        const char *err = arti_last_error(NULL);
        fprintf(stderr, "arti_start failed: %s\n", err ? err : "unknown error");
        return 1;
    }

    /* Report the port arti-c actually bound, not the one requested, so
     * `--socks-port 0` shows the real choice. */
    printf("SOCKS5 proxy on 127.0.0.1:%u\n", (unsigned)arti_socks_port(client));
    printf("bootstrapping onto the Tor network (this can take a minute)...\n");
    fflush(stdout);

    /* There is deliberately no arti_stop() call: this harness is meant to be
     * killed, and SIGINT's default action terminates the process, at which
     * point the OS reclaims the client, the tor connections and the port.
     * Nothing below is reachable by design. */
    for (;;) {
#ifdef _WIN32
        Sleep(1000);
#else
        pause();
#endif
    }
}
