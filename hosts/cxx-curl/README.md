# The libcurl host

This is what a consumer project looks like. It links
[`borink::object_storage`](../../crates/object-storage-cxx) and drives it with
libcurl: `Client` owns a session and the buffers that every request through it
reuses, `curl_host.cc` sends the head that the bridge wrote and feeds the
response back, `main.cc` is a program that gets, puts or removes one object, and `loopback_test.cc` exercises the
whole path against a local HTTP server.

None of this is part of the library. `Limits`, `CollectedHead` and `Client`
are shaped by libcurl in particular — the arena in `CollectedHead` exists
because libcurl retains no header buffer of its own, and a program written
against another HTTP library would not have one. Copy this host, or write your
own against the same bridge; nothing depends on it.

## Building it

The build needs libcurl, CMake and a C++23 compiler, which `flake.nix` at the
repository root provides:

```sh
nix develop --command bash -c '
  cmake -S hosts/cxx-curl -B hosts/cxx-curl/build -DCMAKE_BUILD_TYPE=Debug
  cmake --build hosts/cxx-curl/build
  ctest --test-dir hosts/cxx-curl/build --output-on-failure
'
```

CMake adds the glue crate as a subdirectory, so cargo builds the bridge as
part of the same command. `borink-object-storage-curl` then takes a verb and a
key, reading a written object from standard input and writing a read one to
standard output:

```sh
AZURE_STORAGE_ENDPOINT=https://<account>.blob.core.windows.net \
AZURE_STORAGE_CONTAINER=<container> \
AZURE_STORAGE_ACCESS_TOKEN=$(az account get-access-token \
  --resource https://storage.azure.com/ --query accessToken --output tsv) \
hosts/cxx-curl/build/borink-object-storage-curl get <key>
```
