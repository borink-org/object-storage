# A host: one program that drives `borink::object_storage` with libcurl.
#
# This is what a consumer project looks like. It adds the glue crate as a
# subdirectory, links the target it exports, and finds the HTTP library it
# chose — the only place in this repository that libcurl appears.

cmake_minimum_required(VERSION 3.25)
project(borink-object-storage-curl-host LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 23)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

add_compile_options(-Wall -Wextra -Wpedantic)

find_package(PkgConfig REQUIRED)
pkg_check_modules(libcurl REQUIRED IMPORTED_TARGET libcurl)

add_subdirectory(${CMAKE_CURRENT_SOURCE_DIR}/../../crates/object-storage-cxx
                 ${CMAKE_CURRENT_BINARY_DIR}/object-storage-cxx)

# The host itself: the transport, the buffers and the clock, in one library
# that the program and the test both link.
add_library(curl_host STATIC curl_host.cc host.cc)
target_include_directories(curl_host PUBLIC ${CMAKE_CURRENT_SOURCE_DIR})
target_link_libraries(curl_host PUBLIC borink::object_storage PkgConfig::libcurl)

add_executable(borink-object-storage-curl main.cc)
target_link_libraries(borink-object-storage-curl PRIVATE curl_host)

enable_testing()

add_executable(curl_loopback_test loopback_test.cc)
target_link_libraries(curl_loopback_test PRIVATE curl_host)
add_test(NAME curl_loopback COMMAND curl_loopback_test)
