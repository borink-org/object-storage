# The C++ glue crate, as a target that a consumer links.
#
# Cargo builds the bridge into a static archive and writes the header that
# declares it. This file wraps both, plus the hand-written header in
# `include/`, as `borink::object_storage`. Add it with `add_subdirectory` or
# `FetchContent` and link that target; nothing here needs an HTTP library.

cmake_minimum_required(VERSION 3.25)
project(borink-object-storage-cxx LANGUAGES CXX)

find_package(Threads REQUIRED)

# Cargo names its output directory after the profile, and this project follows
# the CMake build type into it. The crate is a workspace member, so the
# archive lands in the workspace's target directory rather than its own.
if(CMAKE_BUILD_TYPE STREQUAL "Debug")
  set(cargo_arguments "")
  set(cargo_directory debug)
else()
  set(cargo_arguments --release)
  set(cargo_directory release)
endif()

set(workspace_root ${CMAKE_CURRENT_SOURCE_DIR}/../..)
set(bridge_archive ${workspace_root}/target/${cargo_directory}/libborink_object_storage_cxx.a)
set(bridge_include ${workspace_root}/target/cxxbridge)

add_custom_command(
  OUTPUT ${bridge_archive}
  COMMAND cargo build --locked --package borink-object-storage-cxx ${cargo_arguments}
  WORKING_DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR}
  DEPENDS src/lib.rs build.rs Cargo.toml ${workspace_root}/Cargo.lock
  COMMENT "Building the Rust bridge with cargo"
  VERBATIM)
add_custom_target(borink_object_storage_bridge DEPENDS ${bridge_archive})

add_library(borink_object_storage_archive STATIC IMPORTED GLOBAL)
set_target_properties(borink_object_storage_archive PROPERTIES IMPORTED_LOCATION ${bridge_archive})

# What a consumer links: the archive, the generated declarations, the header
# written by hand, and what the Rust standard library needs from the platform.
add_library(borink_object_storage INTERFACE)
add_dependencies(borink_object_storage borink_object_storage_bridge)
target_compile_features(borink_object_storage INTERFACE cxx_std_23)
target_include_directories(borink_object_storage INTERFACE ${bridge_include}
                                                           ${CMAKE_CURRENT_SOURCE_DIR}/include)
target_link_libraries(borink_object_storage INTERFACE borink_object_storage_archive
                                                      Threads::Threads ${CMAKE_DL_LIBS} m)
add_library(borink::object_storage ALIAS borink_object_storage)

# Installing takes the same three things. The generated headers are written by
# the build above, so install them from where cargo put them.
install(DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR}/include/ TYPE INCLUDE)
install(DIRECTORY ${bridge_include}/ TYPE INCLUDE)
install(FILES ${bridge_archive} TYPE LIB)
