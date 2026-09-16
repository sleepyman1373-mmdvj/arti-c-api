# arti-c

A minimal, ABI-stable C wrapper around [Arti](https://gitlab.torproject.org/tpo/core/arti)
2.6.x (via the `arti-client` library). It runs Arti **in-process** and exposes
a local **SOCKS5 proxy** on `127.0.0.1:<port>`.

* No upstream Arti source is modified: arti-c depends on the published
  `arti-client` crates (0.46.x, as shipped with Arti 2.6.x) and only adds a
  new FFI layer.
* The Rust side is compiled as a static library (`libarti_ffi.a`,
  `crate-type = ["staticlib"]`), which CMake links into the final shared
  library: `libarti.so` (Linux), `libarti.dylib` (macOS), or `arti.dll`
  (Windows).
* Only `include/arti.h` is public. Only C types and opaque handles cross the
  ABI boundary, so the header works with GCC, Clang, and MSVC, and from any
  language with a C FFI. The library can be linked normally or loaded at
  runtime with `dlopen()` / `LoadLibrary()`.

## Public API (`include/arti.h`)

```c
arti       *arti_start(const char *data_dir, uint16_t socks_port);
int         arti_is_ready(arti *a);
uint16_t    arti_socks_port(arti *a);
const char *arti_last_error(const arti *a);
void        arti_stop(arti *a);
const char *arti_version(void);
```

* `arti_start` launches the Arti client on an internal thread and begins
  serving SOCKS5 on `127.0.0.1:<socks_port>` (port `0` selects the default
  `9050`). It returns immediately after the listener is bound; it does not
  wait for bootstrap. `data_dir` may be `NULL` for platform-default storage,
  or a directory in which `state/` and `cache/` subdirectories are created.
* `arti_is_ready` returns `1` once the client is fully bootstrapped, `0`
  while still connecting, and `-1` on error.
* `arti_last_error(NULL)` returns the reason for a failed `arti_start`;
  otherwise it returns the most recent error for the given handle.
* `arti_stop` shuts down the client and frees the handle. All functions are
  thread-safe.

## Requirements

* Rust (cargo + rustc), stable toolchain
* CMake >= 3.19
* A C compiler: GCC, Clang, or MSVC
* On Windows with MSVC: the Visual C++ redistributable / build tools
  (Rust's `x86_64-pc-windows-msvc` target)

## Building

```sh
cmake -S arti-c -B arti-c/build -DCMAKE_BUILD_TYPE=Release
cmake --build arti-c/build --parallel
```

Artifacts:

* Shared library: `build/libarti.so` / `build/libarti.dylib` / `build/arti.dll`
* Example binary: `build/simple`

### Windows notes

Open a Visual Studio developer prompt (or use the Visual Studio generator):

```sh
cmake -S arti-c -B arti-c\build -G "Visual Studio 17 2022"
cmake --build arti-c\build --config Release
```

### Cross-compiling the Rust part

```sh
cmake -S arti-c -B build \
    -DARTI_CARGO_TARGET=aarch64-unknown-linux-gnu \
    -DCMAKE_TOOLCHAIN_FILE=...
```

## Installing / packaging

```sh
cmake --install build --prefix /usr/local        # installs lib + include/arti.h
cpack --config build/CPackConfig.cmake           # source/binary archives
```

Installed files:

* `<prefix>/include/arti.h`
* `<prefix>/lib/libarti.so*` (or `dylib`, or `bin/arti.dll` + import lib)
* `<prefix>/bin/simple`

## Using the library

### Normal linking

```c
#include <arti.h>

arti *a = arti_start(NULL, 9050);
while (arti_is_ready(a) == 0) { /* wait */ }
/* use 127.0.0.1:9050 as a SOCKS5 proxy */
arti_stop(a);
```

### Dynamic loading (`dlopen` / `LoadLibrary`)

The same header works; resolve the symbols at runtime:

```c
void *lib = dlopen("libarti.so", RTLD_NOW);   /* or LoadLibrary("arti.dll") */
arti *(*start)(const char *, uint16_t) = dlsym(lib, "arti_start");
/* ... */
```

See `examples/simple.c` for a complete, working program (it performs a
SOCKS5 CONNECT to `example.com:80` and prints the response).

## Design notes

* The Rust crate builds with `crate-type = ["staticlib"]`; its public
  symbols (`arti_*`) have C linkage via `#[no_mangle] pub extern "C"`.
* The SOCKS5 server mirrors upstream's `crates/arti/src/proxy/socks.rs`
  approach, using `tor-socksproto` for the handshake and `TorClient` for
  data streams, but lives entirely inside the wrapper.
* The C API surface is deliberately tiny and uses only C99 types
  (`char *`, `int`, `uint16_t`), so it is stable across compilers and
  platforms. Adding new functions does not break the existing ABI.
* Threads: `arti_start` spawns one OS thread that owns a multi-threaded
  tokio runtime; `arti_stop` joins it and releases everything.

### Logging

Arti logs through the `tracing` crate to stderr. Set `RUST_LOG` to control
verbosity (e.g. `RUST_LOG=debug`); the default is `info`.

### Note on restricted networks

Bootstrap requires unrestricted TCP/TLS access to Tor relays. Networks that
block Tor TLS will cause `arti_is_ready()` to stay `0`; the client keeps
retrying and `arti_stop()` remains responsive.

## License

MIT OR Apache-2.0 (same as Arti). Arti itself is licensed MIT OR Apache-2.0,
with some portions LGPLv3; see the upstream repository for details.
