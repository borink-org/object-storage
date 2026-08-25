# The C++ hosts, and the Rust bridge that they call.
#
# Cargo builds the bridge into a static archive and writes the header that
# declares it. CMake builds one program per host against that archive.

cmake_minimum_required(VERSION 3.25)
project(borink-azure-cxx-hosts LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 23)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

add_compile_options(-Wall -Wextra -Wpedantic)

find_package(PkgConfig REQUIRED)
find_package(Threads REQUIRED)
pkg_check_modules(libcurl REQUIRED IMPORTED_TARGET libcurl)

# Cargo names its output directory after the profile, and this project follows
# the CMake build type into it.
if(CMAKE_BUILD_TYPE STREQUAL "Debug")
  set(cargo_arguments "")
  set(cargo_directory debug)
else()
  set(cargo_arguments --release)
  set(cargo_directory release)
endif()

set(bridge_archive ${CMAKE_CURRENT_SOURCE_DIR}/target/${cargo_directory}/libborink_azure_cxx.a)
set(bridge_include ${CMAKE_CURRENT_SOURCE_DIR}/target/cxxbridge)

add_custom_command(
  OUTPUT ${bridge_archive}
  COMMAND cargo build --locked ${cargo_arguments}
  WORKING_DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR}
  DEPENDS src/lib.rs build.rs Cargo.toml Cargo.lock
  COMMENT "Building the Rust bridge with cargo"
  VERBATIM)
add_custom_target(bridge DEPENDS ${bridge_archive})

add_library(borink_bridge STATIC IMPORTED GLOBAL)
set_target_properties(borink_bridge PROPERTIES IMPORTED_LOCATION ${bridge_archive})
target_include_directories(borink_bridge INTERFACE ${bridge_include} ${CMAKE_CURRENT_SOURCE_DIR}/cpp)
# What the Rust standard library needs from the platform.
target_link_libraries(borink_bridge INTERFACE Threads::Threads ${CMAKE_DL_LIBS} m)

# One library and one program per host. The hosts define the same functions, so
# a program links exactly one of them.
function(add_host name source)
  add_library(${name}_host STATIC ${source} cpp/host.cc)
  add_dependencies(${name}_host bridge)
  target_link_libraries(${name}_host PUBLIC borink_bridge ${ARGN})

  add_executable(borink-azure-${name} cpp/main.cc)
  target_link_libraries(borink-azure-${name} PRIVATE ${name}_host)

  add_executable(${name}_loopback_test cpp/loopback_test.cc)
  target_link_libraries(${name}_loopback_test PRIVATE ${name}_host)
  add_test(NAME ${name}_loopback COMMAND ${name}_loopback_test)
endfunction()

enable_testing()

add_host(curl cpp/curl_host.cc PkgConfig::libcurl)
