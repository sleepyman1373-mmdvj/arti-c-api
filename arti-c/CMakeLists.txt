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
    LANGUAGES C CXX
)

set(CMAKE_C_STANDARD 99)
set(CMAKE_C_STANDARD_REQUIRED ON)

# Only needed by the C++ front-end (examples/arti-proxy.cpp); the shared
# library itself is C. Declared here rather than relying on CMake inferring the
# language from the target's sources, so a missing C++ compiler fails early and
# clearly.
set(CMAKE_CXX_STANDARD 11)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

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
    ${RUST_MANIFEST_DIR}/src/tcp_mark.rs
)

set(RUST_BUILD_ARGS
    build
    --manifest-path ${RUST_MANIFEST_DIR}/Cargo.toml
    --profile ${ARTI_PROFILE}
    --locked
)
if(ARTI_CARGO_TARGET)
    list(APPEND RUST_BUILD_ARGS --target ${ARTI_CARGO_TARGET})
endif()

# We need to know, at *configure* time, the on-disk paths of the native
# static libraries that Rust "sys" crates (sqlite3, etc.) produce as part of
# `cargo build` -- CMake bakes `target_link_libraries()` into the generated
# build files at configure time, so a `file(GLOB ...)` here only ever sees
# what already exists on disk *right now*. On a clean checkout nothing has
# been built yet, so that glob would silently return an empty list forever
# (until someone manually re-runs `cmake` a second time after a build).
#
# To avoid that trap, run the real `cargo build` once, synchronously, during
# configure. This is the same technique used by cargo/CMake integration
# tools such as Corrosion. It makes the very first `cmake -S/-B` invocation
# slower (and is why this project needs cargo/rustc `REQUIRED` above), but
# guarantees the native-library glob below reflects reality. The
# add_custom_command()/add_custom_target() further down still re-invokes
# cargo (which is a fast no-op when nothing changed) so incremental
# `cmake --build` after editing Rust sources keeps working correctly.
message(STATUS "Running initial 'cargo build' so build artifacts can be located (this may take a while)...")
execute_process(
    COMMAND ${CMAKE_COMMAND} -E env CARGO_TARGET_DIR=${RUST_TARGET_DIR}
            ${CARGO_EXECUTABLE} ${RUST_BUILD_ARGS}
    WORKING_DIRECTORY ${RUST_MANIFEST_DIR}
    RESULT_VARIABLE RUST_INITIAL_BUILD_RESULT
)
if(NOT RUST_INITIAL_BUILD_RESULT EQUAL 0)
    message(FATAL_ERROR "Initial 'cargo build' failed (exit code ${RUST_INITIAL_BUILD_RESULT}); see output above.")
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

# Pull EVERY object from libarti_ffi.a into the shared library, not just the
# ones already referenced by shim.c (shim.c is deliberately empty -- see its
# comment). By default, linkers only extract archive members that resolve an
# already-outstanding undefined symbol; since nothing in this C target calls
# arti_start/arti_is_ready/etc., a plain `target_link_libraries(arti PRIVATE
# arti_ffi)` would silently produce a libarti.so/.dylib with *zero* exported
# arti_* symbols on ELF/Mach-O (MSVC/Windows was accidentally fine because
# cmake/arti.def lists the exports explicitly). Force whole-archive
# extraction on every linker family we support instead.
if(MSVC)
    # MSVC is the exception: do NOT use /WHOLEARCHIVE here, even though it is
    # the direct analogue of the flags below.
    #
    # rustc's staticlib output on the MSVC target bundles the import libraries
    # shipped by the `windows-*-msvc` crates as *archive members*, one member
    # per DLL (`kernel32.dll`, `ntoskrnl.exe`, `api-ms-win-core-*.dll`, ...).
    # A real arti_ffi.lib carries thousands of them, with the common ones
    # repeated many times over -- e.g. `kernel32.dll` appears well over a
    # thousand times, because each windows-*-msvc version depends on its own
    # copy. Every one of those members defines the same import-descriptor
    # symbols (`__IMPORT_DESCRIPTOR_kernel32`, `__NULL_IMPORT_DESCRIPTOR_*`,
    # `*_NULL_THUNK_DATA`). Ordinarily that is harmless, since the linker
    # extracts at most one member per undefined symbol and ignores the rest;
    # /WHOLEARCHIVE would force *every* copy in, producing thousands of
    # LNK2005 "already defined" errors and a fatal LNK1169.
    #
    # Windows does not need whole-archive anyway: cmake/arti.def lists all six
    # arti_* exports, and an export list creates exactly the undefined-symbol
    # references that pull the matching archive members in. link.exe then
    # resolves the remaining Windows API imports the usual way, extracting
    # whichever bundled import-library member it needs.
    target_link_libraries(arti PRIVATE arti_ffi)
elseif(APPLE)
    target_link_libraries(arti PRIVATE
        -Wl,-force_load,$<TARGET_FILE:arti_ffi>
    )
else()
    # GNU ld, gold, lld, mold (Linux and other ELF platforms), and MinGW.
    target_link_libraries(arti PRIVATE
        -Wl,--whole-archive $<TARGET_FILE:arti_ffi> -Wl,--no-whole-archive
    )
endif()

# Included here (rather than only further down, next to the install rules) so
# CMAKE_INSTALL_INCLUDEDIR is already defined when the INSTALL_INTERFACE
# generator expression below is evaluated. A generator expression is expanded
# when the target is exported, but `${}` substitution inside it happens
# immediately -- if the variable were still empty at this point, the installed
# target would export an empty include directory and consumers would silently
# not get arti.h on their include path.
include(GNUInstallDirs)

# BUILD_INTERFACE for `add_subdirectory()` consumers, INSTALL_INTERFACE for
# `find_package(arti)` consumers.
target_include_directories(arti PUBLIC
    $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
    $<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>
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
# (GNUInstallDirs is already included above, next to the arti include dirs.)
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
    # Linux and other Unix-likes.
    #
    # arti-client's default TLS backend is `native-tls`, which on Unix
    # (unlike macOS/Windows, where it uses the platform's built-in TLS
    # library and the frameworks/system libs linked above) binds to the
    # system OpenSSL via `openssl-sys`. That crate links OpenSSL
    # dynamically and does NOT vendor/build it, so libssl/libcrypto must be
    # found here explicitly -- rustc has no way to propagate that
    # requirement out of a `staticlib`. Without this, the final
    # libarti.so links but fails to load (or fails to link, depending on
    # the platform) with undefined SSL_*/EVP_* symbols.
    find_package(OpenSSL REQUIRED)
    target_link_libraries(arti PRIVATE OpenSSL::SSL OpenSSL::Crypto dl m)
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
# Command-line SOCKS5 proxy (C++ front-end, used to test the library)
# ---------------------------------------------------------------------------
add_executable(arti-proxy examples/arti-proxy.cpp)
target_link_libraries(arti-proxy PRIVATE arti)
if(MSVC)
    set_target_properties(arti-proxy PROPERTIES MSVC_RUNTIME_LIBRARY "MultiThreadedDLL")
endif()
# No ws2_32 here: unlike `simple`, this program never touches sockets itself.

# ---------------------------------------------------------------------------
# Install / packaging
# ---------------------------------------------------------------------------
# `arti` is installed with an EXPORT set so that `cmake --install` also writes
# the CMake package files consumers need for:
#
#     find_package(arti REQUIRED)
#     target_link_libraries(myapp PRIVATE arti::arti)
#
# The two example executables are installed but deliberately not exported:
# they are programs, not something a consumer links against, and putting an
# executable in an export set would only add noise to artiTargets.cmake.
install(TARGETS arti
    EXPORT   artiTargets
    LIBRARY  DESTINATION ${CMAKE_INSTALL_LIBDIR}
    ARCHIVE  DESTINATION ${CMAKE_INSTALL_LIBDIR}
    RUNTIME  DESTINATION ${CMAKE_INSTALL_BINDIR}
    PUBLIC_HEADER DESTINATION ${CMAKE_INSTALL_INCLUDEDIR}
)
install(TARGETS simple arti-proxy
    RUNTIME  DESTINATION ${CMAKE_INSTALL_BINDIR}
)

# The package files live in <prefix>/lib/cmake/arti/, the standard location
# CMake searches under every entry of CMAKE_PREFIX_PATH, so pointing a
# consumer at this install prefix (-DCMAKE_PREFIX_PATH=<prefix>) is all it
# takes. The NAMESPACE makes the imported target `arti::arti`, which also
# matches the plain `arti` target name used by add_subdirectory() consumers.
install(EXPORT artiTargets
    FILE      artiTargets.cmake
    NAMESPACE arti::
    DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/arti
)

include(CMakePackageConfigHelpers)

# Generate the config file from the template next to this file: @PACKAGE_INIT@
# has to be expanded by configure_package_config_file(), which is what also
# makes the installed targets relocatable (no absolute build-tree paths baked
# in). SameMajorVersion keys compatibility off the major version only, which is
# the same granularity as SOVERSION above. Note the usual 0.x caveat: it makes
# find_package(arti 0.2) succeed against a 0.1.0 install, so bump the major
# version (or switch to ExactVersion) once the ABI is meant to be stable.
configure_package_config_file(
    ${CMAKE_CURRENT_SOURCE_DIR}/cmake/artiConfig.cmake.in
    ${CMAKE_CURRENT_BINARY_DIR}/artiConfig.cmake
    INSTALL_DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/arti
)

write_basic_package_version_file(
    ${CMAKE_CURRENT_BINARY_DIR}/artiConfigVersion.cmake
    VERSION       ${PROJECT_VERSION}
    COMPATIBILITY SameMajorVersion
)

install(FILES
    ${CMAKE_CURRENT_BINARY_DIR}/artiConfig.cmake
    ${CMAKE_CURRENT_BINARY_DIR}/artiConfigVersion.cmake
    DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/arti
)

set(CPACK_PACKAGE_NAME "arti-c")
set(CPACK_PACKAGE_VERSION ${PROJECT_VERSION})
set(CPACK_PACKAGE_VENDOR "The Tor Project")
# NOTE: "CPACK_PACKAGE_LICENSE" is not a variable CPack reads; the correct
# name is CPACK_RESOURCE_FILE_LICENSE, and it must point at an actual file
# (CPack embeds/ships it, e.g. as the license page of a .msi or NSIS
# installer, or LICENSE.txt in a .tar.gz/.zip). We have no bundled LICENSE
# file in this tree, so rather than pointing at a path that doesn't exist
# (which CPack would fail on for generators that require it, e.g. NSIS/WIX),
# just record the SPDX expression in the summary/description instead.
set(CPACK_PACKAGE_DESCRIPTION_SUMMARY
    "Minimal stable C ABI wrapper around Arti (Tor) -- license: MIT OR Apache-2.0")
# CPack defaults CPACK_GENERATOR to a whole platform-dependent list
# (including generators like NSIS/WIX/RPM/DEB that we haven't configured
# the metadata for, and which would fail); pin it to a plain archive
# generator that always works with what's set above.
if(WIN32)
    set(CPACK_GENERATOR "ZIP")
else()
    set(CPACK_GENERATOR "TGZ")
endif()
include(CPack)
