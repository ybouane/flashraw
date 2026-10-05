# Build and install the FlashRaw C/C++ API with CMake (cargo does the compiling).
#
#   cmake -S crates/flashraw-capi -B build/capi -DCMAKE_BUILD_TYPE=Release
#   cmake --build build/capi --config Release
#   cmake --install build/capi --prefix /usr/local
#
# Installs the shared and static libraries, flashraw.h / flashraw.hpp, a pkg-config file
# (flashraw.pc) and a CMake package (find_package(flashraw) -> flashraw::flashraw).
# See docs/c-api.md.
cmake_minimum_required(VERSION 3.16)

file(STRINGS "${CMAKE_CURRENT_SOURCE_DIR}/../../Cargo.toml" _ws_version REGEX "^version = \"[0-9.]+\"")
string(REGEX MATCH "[0-9]+\\.[0-9]+\\.[0-9]+" FLASHRAW_VERSION "${_ws_version}")
project(flashraw VERSION ${FLASHRAW_VERSION} LANGUAGES C)

include(GNUInstallDirs)
include(CMakePackageConfigHelpers)

set(FLASHRAW_CARGO_FEATURES "" CACHE STRING "Comma-separated cargo features of flashraw-capi (e.g. gpu,jpegxl)")
set(FLASHRAW_CARGO_TARGET_DIR "${CMAKE_CURRENT_BINARY_DIR}/cargo" CACHE PATH
    "cargo target directory (point it at the workspace target/ to share build artefacts)")

find_program(CARGO cargo HINTS "$ENV{CARGO_HOME}/bin" "$ENV{HOME}/.cargo/bin" "$ENV{USERPROFILE}/.cargo/bin")
find_program(RUSTC rustc HINTS "$ENV{CARGO_HOME}/bin" "$ENV{HOME}/.cargo/bin" "$ENV{USERPROFILE}/.cargo/bin")
if(NOT CARGO OR NOT RUSTC)
  message(FATAL_ERROR "cargo and rustc are required (https://rustup.rs)")
endif()

# Library file names produced by cargo for this platform.
set(_out "${FLASHRAW_CARGO_TARGET_DIR}/release")
if(WIN32)
  set(FLASHRAW_SHARED_NAME "flashraw_capi.dll")
  set(FLASHRAW_IMPLIB_NAME "flashraw_capi.dll.lib")
  set(FLASHRAW_STATIC_NAME "flashraw_capi.lib")
elseif(APPLE)
  set(FLASHRAW_SHARED_NAME "libflashraw_capi.dylib")
  set(FLASHRAW_STATIC_NAME "libflashraw_capi.a")
else()
  set(FLASHRAW_SHARED_NAME "libflashraw_capi.so")
  set(FLASHRAW_STATIC_NAME "libflashraw_capi.a")
endif()

# System libraries a static Rust library needs on this platform, as reported by rustc.
set(_probe_dir "${CMAKE_CURRENT_BINARY_DIR}/native-libs-probe")
file(MAKE_DIRECTORY "${_probe_dir}")
file(WRITE "${_probe_dir}/probe.rs" "")
execute_process(
  COMMAND "${RUSTC}" --crate-type staticlib --crate-name probe --print native-static-libs
          -o "${_probe_dir}/probe.lib" "${_probe_dir}/probe.rs"
  ERROR_VARIABLE _rustc_out OUTPUT_QUIET RESULT_VARIABLE _rustc_rc)
string(REGEX MATCH "native-static-libs: ([^\r\n]*)" _ "${_rustc_out}")
set(FLASHRAW_NATIVE_LIBS_RAW "${CMAKE_MATCH_1}")
string(STRIP "${FLASHRAW_NATIVE_LIBS_RAW}" FLASHRAW_NATIVE_LIBS_RAW)
set(FLASHRAW_NATIVE_LIBS "")
separate_arguments(_tokens NATIVE_COMMAND "${FLASHRAW_NATIVE_LIBS_RAW}")
set(_framework OFF)
foreach(_t IN LISTS _tokens)
  if(_framework)
    list(APPEND FLASHRAW_NATIVE_LIBS "-framework ${_t}")
    set(_framework OFF)
  elseif(_t STREQUAL "-framework")
    set(_framework ON)
  elseif(_t MATCHES "^-l(.+)$")
    list(APPEND FLASHRAW_NATIVE_LIBS "${CMAKE_MATCH_1}")
  elseif(_t MATCHES "^(.+)\\.lib$")
    list(APPEND FLASHRAW_NATIVE_LIBS "${CMAKE_MATCH_1}")
  endif() # linker flags such as /defaultlib:msvcrt are implied by the toolchain
endforeach()
list(REMOVE_DUPLICATES FLASHRAW_NATIVE_LIBS)
message(STATUS "flashraw: static library needs: ${FLASHRAW_NATIVE_LIBS}")

set(_cargo_args build --release -p flashraw-capi
    --manifest-path "${CMAKE_CURRENT_SOURCE_DIR}/Cargo.toml"
    --target-dir "${FLASHRAW_CARGO_TARGET_DIR}")
if(FLASHRAW_CARGO_FEATURES)
  list(APPEND _cargo_args --features "${FLASHRAW_CARGO_FEATURES}")
endif()
set(_byproducts "${_out}/${FLASHRAW_SHARED_NAME}" "${_out}/${FLASHRAW_STATIC_NAME}")
if(WIN32)
  list(APPEND _byproducts "${_out}/${FLASHRAW_IMPLIB_NAME}")
endif()
# cargo tracks the Rust sources itself; run it on every build (a no-op when up to date).
add_custom_target(flashraw_cargo ALL
  COMMAND "${CARGO}" ${_cargo_args}
  BYPRODUCTS ${_byproducts}
  WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}"
  COMMENT "cargo build --release -p flashraw-capi"
  USES_TERMINAL VERBATIM)

# In-tree targets (add_subdirectory users): same names as the installed package.
add_library(flashraw_shared SHARED IMPORTED GLOBAL)
add_library(flashraw_static STATIC IMPORTED GLOBAL)
set_target_properties(flashraw_shared PROPERTIES
  IMPORTED_LOCATION "${_out}/${FLASHRAW_SHARED_NAME}"
  INTERFACE_INCLUDE_DIRECTORIES "${CMAKE_CURRENT_SOURCE_DIR}/include")
if(WIN32)
  set_target_properties(flashraw_shared PROPERTIES IMPORTED_IMPLIB "${_out}/${FLASHRAW_IMPLIB_NAME}")
endif()
set_target_properties(flashraw_static PROPERTIES
  IMPORTED_LOCATION "${_out}/${FLASHRAW_STATIC_NAME}"
  INTERFACE_INCLUDE_DIRECTORIES "${CMAKE_CURRENT_SOURCE_DIR}/include"
  INTERFACE_LINK_LIBRARIES "${FLASHRAW_NATIVE_LIBS}")
add_dependencies(flashraw_shared flashraw_cargo)
add_dependencies(flashraw_static flashraw_cargo)
add_library(flashraw::flashraw_shared ALIAS flashraw_shared)
add_library(flashraw::flashraw_static ALIAS flashraw_static)
add_library(flashraw INTERFACE)
target_link_libraries(flashraw INTERFACE flashraw_shared)
add_library(flashraw::flashraw ALIAS flashraw)

# ---- install ----
install(FILES include/flashraw.h include/flashraw.hpp DESTINATION "${CMAKE_INSTALL_INCLUDEDIR}")
install(FILES "${_out}/${FLASHRAW_STATIC_NAME}" DESTINATION "${CMAKE_INSTALL_LIBDIR}")
if(WIN32)
  install(FILES "${_out}/${FLASHRAW_SHARED_NAME}" DESTINATION "${CMAKE_INSTALL_BINDIR}")
  install(FILES "${_out}/${FLASHRAW_IMPLIB_NAME}" DESTINATION "${CMAKE_INSTALL_LIBDIR}")
else()
  install(PROGRAMS "${_out}/${FLASHRAW_SHARED_NAME}" DESTINATION "${CMAKE_INSTALL_LIBDIR}")
endif()

set(_cmake_dir "${CMAKE_INSTALL_LIBDIR}/cmake/flashraw")
configure_package_config_file(cmake/flashrawConfig.cmake.in "${CMAKE_CURRENT_BINARY_DIR}/flashrawConfig.cmake"
  INSTALL_DESTINATION "${_cmake_dir}"
  PATH_VARS CMAKE_INSTALL_INCLUDEDIR CMAKE_INSTALL_LIBDIR CMAKE_INSTALL_BINDIR)
write_basic_package_version_file("${CMAKE_CURRENT_BINARY_DIR}/flashrawConfigVersion.cmake"
  VERSION ${PROJECT_VERSION} COMPATIBILITY SameMajorVersion)
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/flashrawConfig.cmake" "${CMAKE_CURRENT_BINARY_DIR}/flashrawConfigVersion.cmake"
  DESTINATION "${_cmake_dir}")

if(NOT WIN32)
  string(REPLACE ";" " -l" _pc_private "${FLASHRAW_NATIVE_LIBS}")
  if(_pc_private)
    set(FLASHRAW_PC_LIBS_PRIVATE "-l${_pc_private}")
  endif()
  string(REPLACE "-l-framework " "-framework " FLASHRAW_PC_LIBS_PRIVATE "${FLASHRAW_PC_LIBS_PRIVATE}")
  configure_file(cmake/flashraw.pc.in "${CMAKE_CURRENT_BINARY_DIR}/flashraw.pc" @ONLY)
  install(FILES "${CMAKE_CURRENT_BINARY_DIR}/flashraw.pc" DESTINATION "${CMAKE_INSTALL_LIBDIR}/pkgconfig")
endif()
