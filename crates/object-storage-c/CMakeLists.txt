# The C ABI crate, as a target that a consumer links.
#
# Cargo builds this crate into a static archive, and `include/` holds the
# committed header that declares it. This file wraps both, plus the
# header-only C++ layer beside it, as `borink::object_storage`. Add it with
# `add_subdirectory` or `FetchContent` and link that target; nothing here needs
# an HTTP library, and a C program needs no C++ compiler.

cmake_minimum_required(VERSION 3.25)
project(borink-object-storage-c LANGUAGES C)

find_package(Threads REQUIRED)

# Cargo names its output directory after the profile, and this project follows
# the CMake build type into it. The crate is a workspace member, so the archive
# lands in the workspace's target directory rather than its own.
if(CMAKE_BUILD_TYPE STREQUAL "Debug")
  set(cargo_arguments "")
  set(cargo_directory debug)
else()
  set(cargo_arguments --release)
  set(cargo_directory release)
endif()

set(workspace_root ${CMAKE_CURRENT_SOURCE_DIR}/../..)
set(abi_archive ${workspace_root}/target/${cargo_directory}/libborink_object_storage_c.a)

add_custom_command(
  OUTPUT ${abi_archive}
  COMMAND cargo build --locked --package borink-object-storage-c ${cargo_arguments}
  WORKING_DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR}
  DEPENDS src/lib.rs src/panic.rs Cargo.toml ${workspace_root}/Cargo.lock
  COMMENT "Building the Rust archive with cargo"
  VERBATIM)
add_custom_target(borink_object_storage_build DEPENDS ${abi_archive})

add_library(borink_object_storage_archive STATIC IMPORTED GLOBAL)
set_target_properties(borink_object_storage_archive PROPERTIES IMPORTED_LOCATION ${abi_archive})

# What a consumer links: the archive, the two headers, and what the Rust
# standard library needs from the platform. The C++ layer is header-only, so no
# C++ compiler is required to build this target.
add_library(borink_object_storage INTERFACE)
add_dependencies(borink_object_storage borink_object_storage_build)
target_include_directories(borink_object_storage INTERFACE ${CMAKE_CURRENT_SOURCE_DIR}/include)
target_link_libraries(borink_object_storage INTERFACE borink_object_storage_archive
                                                      Threads::Threads ${CMAKE_DL_LIBS} m)
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

install(DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR}/include/ TYPE INCLUDE)
install(FILES ${abi_archive} TYPE LIB)
