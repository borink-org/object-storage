# The C ABI crate, as a target that a consumer links.
#
# `cargo cbuild` produces the archive, the header directory and a pkg-config
# file describing both. This file wraps them as `borink::object_storage`. Add
# it with `add_subdirectory` or `FetchContent` and link that target; nothing
# here needs an HTTP library, and a C program needs no C++ compiler.
#
# There is no install rule. `cargo cinstall --prefix=...` installs, and
# pkg-config finds what it wrote. See docs/PACKAGING.md.

cmake_minimum_required(VERSION 3.25)
project(borink-object-storage-c LANGUAGES C)

find_package(PkgConfig REQUIRED)
find_program(CARGO_EXECUTABLE cargo REQUIRED)
find_program(RUSTC_EXECUTABLE rustc REQUIRED)

# cargo-c names its output directory after the profile, and writes under the
# target triple even for a host build. This project follows the CMake build
# type into it, and asks the compiler for the triple.
if(CMAKE_BUILD_TYPE STREQUAL "Debug")
  set(cargo_arguments "")
  set(cargo_directory debug)
else()
  set(cargo_arguments --release)
  set(cargo_directory release)
endif()

execute_process(
  COMMAND ${RUSTC_EXECUTABLE} -vV
  OUTPUT_VARIABLE rustc_verbose_version
  COMMAND_ERROR_IS_FATAL ANY)
string(REGEX MATCH "host: ([^\n]+)" _ ${rustc_verbose_version})
set(rust_host_triple ${CMAKE_MATCH_1})

set(workspace_root ${CMAKE_CURRENT_SOURCE_DIR}/../..)
set(cargo_output_directory ${workspace_root}/target/${rust_host_triple}/${cargo_directory})
set(abi_archive ${cargo_output_directory}/libborink_object_storage_c.a)

set(cbuild_command ${CARGO_EXECUTABLE} cbuild --locked
                   --package borink-object-storage-c
                   --library-type staticlib ${cargo_arguments})

# Once at configure time, because the pkg-config file read below is something
# the build produces. Once again as a build rule, so editing the Rust source
# rebuilds the archive.
execute_process(
  COMMAND ${cbuild_command}
  WORKING_DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR}
  COMMAND_ERROR_IS_FATAL ANY)

add_custom_command(
  OUTPUT ${abi_archive}
  COMMAND ${cbuild_command}
  WORKING_DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR}
  DEPENDS src/lib.rs src/panic.rs Cargo.toml ${workspace_root}/Cargo.lock
  COMMENT "Building the Rust archive with cargo cbuild"
  VERBATIM)
add_custom_target(borink_object_storage_build DEPENDS ${abi_archive})

# What a consumer links: the archive, the header directory beside it, and the
# native libraries rustc reported. All three come out of the pkg-config file.
set(ENV{PKG_CONFIG_PATH} ${cargo_output_directory})
pkg_check_modules(borink_object_storage_c REQUIRED IMPORTED_TARGET
                  borink-object-storage-c-uninstalled)

add_library(borink_object_storage INTERFACE)
add_dependencies(borink_object_storage borink_object_storage_build)
target_link_libraries(borink_object_storage INTERFACE PkgConfig::borink_object_storage_c)
add_library(borink::object_storage ALIAS borink_object_storage)

# The C program that checks this crate's ABI against what the C compiler
# computes. It is built and run only when this project is the top of the build.
if(PROJECT_IS_TOP_LEVEL)
  enable_testing()
  add_executable(borink_object_storage_abi_test tests/abi.c)
  target_compile_features(borink_object_storage_abi_test PRIVATE c_std_11)
  target_compile_options(borink_object_storage_abi_test PRIVATE -Wall -Wextra -Wpedantic)
  target_link_libraries(borink_object_storage_abi_test PRIVATE borink::object_storage)
  add_test(NAME borink_object_storage_abi COMMAND borink_object_storage_abi_test)
endif()
