# C and C++ examples of the FlashRaw C API, built against an installed FlashRaw:
#
#   cmake -S crates/flashraw-capi -B build/capi -DCMAKE_BUILD_TYPE=Release
#   cmake --build build/capi --config Release
#   cmake --install build/capi --prefix "$PWD/build/prefix" --config Release
#   cmake -S examples -B build/examples -DCMAKE_PREFIX_PATH="$PWD/build/prefix"
#   cmake --build build/examples --config Release
#   ctest --test-dir build/examples -C Release --output-on-failure
cmake_minimum_required(VERSION 3.16)
project(flashraw_examples LANGUAGES C CXX)

find_package(flashraw 0.1 REQUIRED)

# C example, linked statically (no runtime dependency on the FlashRaw shared library).
add_executable(flashraw_info c/flashraw_info.c)
set_target_properties(flashraw_info PROPERTIES C_STANDARD 99 C_STANDARD_REQUIRED ON)
target_link_libraries(flashraw_info PRIVATE flashraw::flashraw_static)

# C++17 example, linked against the shared library.
add_executable(flashraw_decode cpp/flashraw_decode.cpp)
set_target_properties(flashraw_decode PROPERTIES CXX_STANDARD 17 CXX_STANDARD_REQUIRED ON)
target_link_libraries(flashraw_decode PRIVATE flashraw::flashraw)
if(CMAKE_CXX_COMPILER_ID MATCHES "GNU|Clang")
  target_compile_options(flashraw_info PRIVATE -Wall -Wextra -Werror)
  target_compile_options(flashraw_decode PRIVATE -Wall -Wextra -Wshadow -Werror)
  find_package(Threads REQUIRED)
  target_link_libraries(flashraw_decode PRIVATE Threads::Threads)
elseif(MSVC)
  target_compile_definitions(flashraw_info PRIVATE _CRT_SECURE_NO_WARNINGS)
  target_compile_options(flashraw_decode PRIVATE /EHsc)
endif()
if(WIN32)
  # Windows finds DLLs next to the executable: copy the FlashRaw DLL there.
  add_custom_command(TARGET flashraw_decode POST_BUILD
    COMMAND "${CMAKE_COMMAND}" -E copy_if_different "$<TARGET_FILE:flashraw::flashraw_shared>"
            "$<TARGET_FILE_DIR:flashraw_decode>")
endif()

# Run the examples on a small DNG from the repository.
enable_testing()
set(FLASHRAW_EXAMPLE_INPUT "${CMAKE_CURRENT_SOURCE_DIR}/../npm/tests/fixtures/canon-a410-chdk.dng"
    CACHE FILEPATH "RAW file the example tests decode")
add_test(NAME c_info COMMAND flashraw_info "${FLASHRAW_EXAMPLE_INPUT}" "${CMAKE_CURRENT_BINARY_DIR}/c.ppm")
add_test(NAME cpp_decode COMMAND flashraw_decode "${FLASHRAW_EXAMPLE_INPUT}" "${CMAKE_CURRENT_BINARY_DIR}/cpp.ppm"
         "${CMAKE_CURRENT_BINARY_DIR}/preview.bin")
