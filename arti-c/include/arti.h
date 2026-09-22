/* arti.h - Public C API for arti-c, a minimal C ABI wrapper around Arti.
 *
 * This is the only header that is part of arti-c's public interface.
 * Only C types and opaque handles cross the ABI boundary, so this header
 * is usable from C, C++, and any language with a C FFI, with any compiler
 * (GCC, Clang, MSVC).
 *
 * The library runs Arti in-process and exposes a local SOCKS5 proxy on
 * 127.0.0.1:<port>. All functions are thread-safe unless documented
 * otherwise.
 */
#ifndef ARTI_H
#define ARTI_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque client handle; returned by arti_start(), released by arti_stop(). */
typedef struct arti arti;

/*
 * Start the Arti client in-process and begin serving a SOCKS5 proxy on
 * 127.0.0.1:<socks_port>.  If socks_port is 0, the default port 9050 is
 * used.
 *
 * data_dir may be NULL to use platform-default Arti storage directories.
 * Otherwise it names a directory that will hold persistent Tor state
 * (state/) and cache (cache/) subdirectories; it is created if missing.
 *
 * Returns an opaque handle, or NULL on failure (reason via
 * arti_last_error(NULL)).  Starting does not wait for bootstrap; poll
 * arti_is_ready() for that.  The handle must be released with exactly one
 * call to arti_stop().
 */
arti *arti_start(const char *data_dir, uint16_t socks_port);

/*
 * Start the Arti client exactly as arti_start() does, but stamp
 * outbound_mark on every socket Arti opens, via SO_MARK.
 *
 * A host that diverts all outbound traffic into a transparent proxy needs
 * this: Arti's own connections to the Tor network have to be exempt from
 * those rules, or they are diverted straight back into the tunnel Arti is
 * carrying. The mark is the caller's to choose -- the firewall rules use it
 * to identify the traffic to pass through untouched.
 *
 * Pass 0 to disable marking, which is identical to arti_start(). SO_MARK is
 * Linux-only: elsewhere a non-zero mark starts the client successfully but
 * makes every connection attempt fail, so request a mark only on a platform
 * where you could have set one yourself.
 */
arti *arti_start_with_mark(const char *data_dir,
                           uint16_t socks_port,
                           uint32_t outbound_mark);

/*
 * Return 1 if the client has fully bootstrapped onto the Tor network,
 * 0 if it is still connecting, and -1 on error (NULL handle, or bootstrap
 * failed; see arti_last_error()).
 */
int arti_is_ready(arti *a);

/*
 * Return the port the SOCKS5 proxy is actually listening on
 * (127.0.0.1:<port>), or 0 on error / invalid handle.
 */
uint16_t arti_socks_port(arti *a);

/*
 * Return the most recent error message for this handle as a NUL-terminated
 * string, or NULL if there is none.  With a NULL handle, return the error
 * from the most recent failed arti_start() call.
 *
 * The returned pointer is owned by, and lives in storage private to, the
 * calling thread (like strerror() on most platforms). It remains valid
 * until that same thread calls arti_last_error() again (on any handle), and
 * must not be used from a different thread or freed.
 */
const char *arti_last_error(const arti *a);

/*
 * Shut the client down, free the handle, and release all resources.
 * After this call the handle is invalid and must not be used again.
 * Passing NULL is a no-op.  Blocks until shutdown completes.
 */
void arti_stop(arti *a);

/*
 * Return version information as a static, NUL-terminated string.
 * The pointer is valid for the lifetime of the process; do not free it.
 */
const char *arti_version(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* ARTI_H */
