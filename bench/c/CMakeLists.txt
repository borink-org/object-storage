# The listing bench from C and from C++, against the ABI crate's archive.
# Build the archive with CARGO_PROFILE_RELEASE_LTO=true and
# CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 set, the way the Rust bench is built.
cmake_minimum_required(VERSION 3.25)
project(borink_object_storage_bench_c LANGUAGES C CXX)

add_subdirectory(../../crates/object-storage-c object-storage-c)

add_executable(bench_c bench.c)
target_compile_features(bench_c PRIVATE c_std_11)
target_compile_options(bench_c PRIVATE -O2 -Wall -Wextra)
target_link_libraries(bench_c PRIVATE borink::object_storage)

add_executable(bench_cxx bench.cc)
target_compile_features(bench_cxx PRIVATE cxx_std_23)
target_compile_options(bench_cxx PRIVATE -O2 -Wall -Wextra)
target_link_libraries(bench_cxx PRIVATE borink::object_storage)
