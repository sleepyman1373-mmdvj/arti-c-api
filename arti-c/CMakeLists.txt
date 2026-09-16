# arti-c: a minimal stable C ABI wrapper around Arti
#
# Links the Rust static library (libarti_ffi.a / arti_ffi.lib, built by
# Cargo) into a final shared library (libarti.so / libarti.dylib / arti.dll)
# and installs it together with the single public header include/arti.h.
#
# Requirements: CMake >= 3.19, a C compiler (GCC / Clang / MSVC), and
# cargo + rustc on the PATH.

cmake_minimum_required(VERSION 3.19)

project(arti-c
    VERSION 0.1.0
    DESCRIPTION "Minimal stable C ABI wrapper around Arti with a built-in SOCKS5 proxy"
    LANGUAGES C
)

set(CMAKE_C_STANDARD 99)
set(CMAKE_C_STANDARD_REQUIRED ON)

# ---------------------------------------------------------------------------
# Options
# ---------------------------------------------------------------------------
set(ARTI_PROFILE "release" CACHE STRING "Cargo profile to build (release or debug)")
set(ARTI_CARGO_TARGET "" CACHE STRING "Optional cargo target triple (for cross-compilation)")

# ---------------------------------------------------------------------------
# Locate the Rust toolchain
# ---------------------------------------------------------------------------
find_program(CARGO_EXECUTABLE cargo REQUIRED)
find_program(RUSTC_EXECUTABLE rustc REQUIRED)
message(STATUS "cargo:   ${CARGO_EXECUTABLE}")
message(STATUS "rustc:   ${RUSTC_EXECUTABLE}")

set(RUST_MANIFEST_DIR ${CMAKE_CURRENT_SOURCE_DIR})
set(RUST_LIB_NAME ${CMAKE_STATIC_LIBRARY_PREFIX}arti_ffi${CMAKE_STATIC_LIBRARY_SUFFIX})

if(ARTI_CARGO_TARGET)
    set(RUST_TARGET_SUBDIR ${ARTI_CARGO_TARGET}/${ARTI_PROFILE})
else()
    set(RUST_TARGET_SUBDIR ${ARTI_PROFILE})
endif()

# Build out-of-tree so we never modify upstream Arti sources.
set(RUST_TARGET_DIR ${CMAKE_CURRENT_BINARY_DIR}/rust-target)
set(RUST_STATIC_LIB ${RUST_TARGET_DIR}/${RUST_TARGET_SUBDIR}/${RUST_LIB_NAME})

set(RUST_SOURCES
    ${RUST_MANIFEST_DIR}/Cargo.toml
    ${RUST_MANIFEST_DIR}/Cargo.lock
    ${RUST_MANIFEST_DIR}/src/lib.rs
    ${RUST_MANIFEST_DIR}/src/socks.rs
)

set(RUST_BUILD_ARGS
    build
    --manifest-path ${RUST_MANIFEST_DIR}/Cargo.toml
    --profile ${ARTI_PROFILE}
)
if(ARTI_CARGO_TARGET)
    list(APPEND RUST_BUILD_ARGS --target ${ARTI_CARGO_TARGET})
else()
    list(APPEND RUST_BUILD_ARGS --locked)
endif()

add_custom_command(
    OUTPUT ${RUST_STATIC_LIB}
    COMMAND ${CMAKE_COMMAND} -E env CARGO_TARGET_DIR=${RUST_TARGET_DIR}
            ${CARGO_EXECUTABLE} ${RUST_BUILD_ARGS}
    COMMAND ${CMAKE_COMMAND} -E echo "Rust static library built: ${RUST_STATIC_LIB}"
    DEPENDS ${RUST_SOURCES}
    COMMENT "Building Rust static library (cargo build --profile ${ARTI_PROFILE})"
    USES_TERMINAL
    VERBATIM
)
add_custom_target(arti_ffi_rust ALL DEPENDS ${RUST_STATIC_LIB})

# Import the Rust static library as a CMake target.
add_library(arti_ffi STATIC IMPORTED GLOBAL)
add_dependencies(arti_ffi arti_ffi_rust)
set_target_properties(arti_ffi PROPERTIES IMPORTED_LOCATION ${RUST_STATIC_LIB})

# ---------------------------------------------------------------------------
# The shared library that consumers link against
# ---------------------------------------------------------------------------
add_library(arti SHARED
    src/shim.c
)
if(WIN32)
    # Windows-style linkers need an explicit export list; symbols from the
    # Rust static library are not exported automatically. On Unix, the
    # #[no_mangle] symbols are exported by default.
    target_sources(arti PRIVATE cmake/arti.def)
endif()
add_dependencies(arti arti_ffi)
target_link_libraries(arti PRIVATE arti_ffi)
target_include_directories(arti PUBLIC
    $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
)

# The Rust static library is always built in the release profile and links
# against the dynamic release CRT; make the C side match so the CRT agrees
# even for Debug configurations.
if(MSVC)
    set_target_properties(arti PROPERTIES MSVC_RUNTIME_LIBRARY "MultiThreadedDLL")
endif()

set_target_properties(arti PROPERTIES
    VERSION   ${PROJECT_VERSION}
    SOVERSION ${PROJECT_VERSION_MAJOR}
    PUBLIC_HEADER include/arti.h
)

# Rust static libraries need the C++ runtime on some configurations (LLVM
# toolchain components); harmless where unused.
include(GNUInstallDirs)
find_package(Threads REQUIRED)
target_link_libraries(arti PRIVATE Threads::Threads)

if(WIN32)
    if(MSVC)
        target_link_libraries(arti PRIVATE
            ws2_32 mswsock userenv ntdll bcrypt crypt32 secur32
            advapi32 kernel32 dnsapi iphlpapi ncrypt
        )
    else()
        # MinGW / Clang on Windows
        target_link_libraries(arti PRIVATE
            ws2_32 mswsock userenv ntdll bcrypt advapi32
            kernel32 dnsapi iphlpapi
        )
    endif()
elseif(APPLE)
    target_link_libraries(arti PRIVATE
        "-framework Security"
        "-framework CoreFoundation"
        "-framework SystemConfiguration"
    )
else()
    # Linux and other Unix-likes
    target_link_libraries(arti PRIVATE dl m)
endif()

# Native static archives produced by Rust "sys" crates (sqlite3, zstd, lzma,
# ...) during the cargo build. Cargo does not propagate these to consumers of
# a `staticlib`, so link them here.
file(GLOB RUST_NATIVE_LIBS
    ${RUST_TARGET_DIR}/${RUST_TARGET_SUBDIR}/build/*\__*/out/*${CMAKE_STATIC_LIBRARY_SUFFIX}
    ${RUST_TARGET_DIR}/${RUST_TARGET_SUBDIR}/build/*/out/*${CMAKE_STATIC_LIBRARY_SUFFIX}
)
list(FILTER RUST_NATIVE_LIBS EXCLUDE REGEX "sqlite3_bindgen|dlsym")
if(RUST_NATIVE_LIBS)
    message(STATUS "Rust sys-crate native libraries: ${RUST_NATIVE_LIBS}")
    target_link_libraries(arti PRIVATE ${RUST_NATIVE_LIBS})
endif()

# Visibility: only the Rust-exported symbols (and the header API) should be
# public in the shared object.
if(NOT MSVC)
    target_compile_options(arti PRIVATE -fvisibility=hidden)
endif()

# ---------------------------------------------------------------------------
# Example
# ---------------------------------------------------------------------------
add_executable(simple examples/simple.c)
target_link_libraries(simple PRIVATE arti)
if(MSVC)
    set_target_properties(simple PROPERTIES MSVC_RUNTIME_LIBRARY "MultiThreadedDLL")
endif()
if(WIN32)
    target_link_libraries(simple PRIVATE ws2_32)
endif()

# ---------------------------------------------------------------------------
# Install / packaging
# ---------------------------------------------------------------------------
install(TARGETS arti simple
    LIBRARY  DESTINATION ${CMAKE_INSTALL_LIBDIR}
    ARCHIVE  DESTINATION ${CMAKE_INSTALL_LIBDIR}
    RUNTIME  DESTINATION ${CMAKE_INSTALL_BINDIR}
    PUBLIC_HEADER DESTINATION ${CMAKE_INSTALL_INCLUDEDIR}
)

set(CPACK_PACKAGE_NAME "arti-c")
set(CPACK_PACKAGE_VERSION ${PROJECT_VERSION})
set(CPACK_PACKAGE_DESCRIPTION_SUMMARY "Minimal stable C ABI wrapper around Arti (Tor)")
set(CPACK_PACKAGE_VENDOR "The Tor Project")
set(CPACK_PACKAGE_LICENSE "MIT OR Apache-2.0")
include(CPack)
