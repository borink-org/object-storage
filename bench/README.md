# Bench

Measures the listing reader over one generated Azure page of 5000 objects (about 3.4 MB). Run it by hand when a change is meant to move the numbers, or might. Nothing runs it automatically and nothing stores its results; paste a table into the commit message or the pull request when the numbers matter.

```
cd bench
cargo run --release                   # wall time, instructions and cycles
cargo run --release -- --callgrind    # exact instruction counts, needs valgrind
cargo run --release -- --rounds 200
```

Instruction counts are the number to compare between versions. Wall time on the same machine moves by 5 to 20% between runs for reasons that have nothing to do with the code, so a real regression of a few percent hides in it. Instructions retired do not move that way. Cycles sit in between: they show a change that costs no instructions, such as a cache miss or a mispredicted branch, but they are not stable enough to gate on.

Two rows: `fill_listing`, the whole read of the page, and `fill_listing_with`, the same read keeping three properties of every object. The two should cost the same; the second exists to show that they do.

## From C and C++

`c/` holds the same measurement made through the ABI crate, from C and from C++, so that the cost of the boundary is a number rather than a guess. Both programs read a page this bench wrote and report the same columns.

```
cargo run --release -- --write target/azure-page.xml
export CARGO_PROFILE_RELEASE_LTO=true CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
nix develop --command cmake -S c -B c/build -DCMAKE_BUILD_TYPE=Release
nix develop --command cmake --build c/build
c/build/bench_c target/azure-page.xml 100
c/build/bench_cxx target/azure-page.xml 100
```

The two environment variables build the archive the way this bench's own binary is built. Without them the workspace's release profile applies, which has no link-time optimisation and sixteen codegen units, and the rows read slower for a reason that has nothing to do with the boundary. The counters are opened the same way as here, so the same capability applies.

## Hardware counters

The bench reads the counters through `perf_event_open`, so it needs no tool installed. The kernel may refuse that to an unprivileged process, depending on `kernel.perf_event_paranoid`. When it does, the bench prints the command it is about to run, asks `sudo` to grant the built binary `CAP_PERFMON` with `setcap`, and runs itself again. That is a capability on the file, not a system setting: it lasts until the next build replaces the binary, and nothing else on the machine changes. If you decline the prompt, the bench prints wall time only.

The bench pins itself to CPU 0 and counts the thread that runs it, in user mode only. On a CPU with two kinds of core the counter cannot follow a thread from one kind to the other, which is one reason for the pin. A round in which the counter was not running the whole time is left out of the medians and reported by count. Counts differ by a few hundred instructions between identical runs, which is noise at the scale of a page (tens of millions).

The dev shell in `flake.nix` carries `perf` for measuring anything the bench does not; `perf stat` needs the same capability or a lower `perf_event_paranoid`.

## Callgrind

`--callgrind` runs this binary under valgrind once per row and reports the instructions per round inside the measured call, which is the same region the hardware counters cover, so the two numbers are comparable. Callgrind counts every instruction, so the number is exact and the same on every run, at about a fiftieth of native speed. The dev shell carries `valgrind`; run it from `nix develop`. Use it when two versions differ by less than the hardware counter's noise, or on a machine where the capability cannot be granted.
