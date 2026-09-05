// The measurement in bench.c, made through the C++ header. The second row
// copies the three values of every entry into a struct of the program's own,
// the way the Rust bench's entry type does, so the helpers are measured too.
//
//     ./bench_cxx page.xml [rounds]

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <iterator>
#include <string_view>
#include <vector>

#include <linux/perf_event.h>
#include <sched.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <unistd.h>

#include "borink/object_storage/core.hpp"

namespace {

constexpr std::size_t Capacity = 8192;

int counter(unsigned config) {
    perf_event_attr attr{};
    attr.type = PERF_TYPE_HARDWARE;
    attr.size = sizeof attr;
    attr.config = config;
    attr.disabled = 1;
    attr.exclude_kernel = 1;
    attr.exclude_hv = 1;
    return static_cast<int>(syscall(SYS_perf_event_open, &attr, 0, -1, -1, 0));
}

std::uint64_t count(int fd) {
    std::uint64_t value = 0;
    if (fd >= 0 && read(fd, &value, sizeof value) != static_cast<ssize_t>(sizeof value)) {
        value = 0;
    }
    return value;
}

// One entry as a C++ program keeps it: the key and three properties.
struct Picked {
    std::string_view key;
    borink::MaybeU64 size;
    borink::MaybeBytes created;
    borink::MaybeBytes tier;
    borink::MaybeBytes md5;
};

struct Round {
    borink::Session session;
    std::vector<std::uint8_t> work;
    std::vector<borink::ListEntry> entries;
    std::vector<borink::MaybeBytes> values;
    std::vector<Picked> picked;
    borink::PropertySet wanted;
};

std::size_t plain(Round &r) {
    const borink::Fill fill =
        borink_fill_listing(&r.session, borink::into(r.work), r.entries.data(), r.entries.size());
    return borink::entries_of(r.entries, fill).size();
}

std::size_t with(Round &r) {
    const borink::Fill fill = borink_fill_listing_with(&r.session, borink::into(r.work),
                                                       r.entries.data(), r.entries.size(),
                                                       r.wanted, r.values.data(), r.values.size());
    const std::span<const borink::ListEntry> read = borink::entries_of(r.entries, fill);
    for (std::size_t i = 0; i < read.size(); i++) {
        const std::span<const borink::MaybeBytes> row = borink::values_of(r.values, r.wanted, i);
        r.picked[i] = Picked{
            borink::text_of(read[i].key),
            read[i].size,
            borink::value(row, r.wanted, borink::BlobPropertyCreationTime),
            borink::value(row, r.wanted, borink::BlobPropertyAccessTier),
            borink::value(row, r.wanted, borink::BlobPropertyContentMd5),
        };
    }
    return read.size();
}

void measure(const char *name, const std::vector<std::uint8_t> &page, int rounds, Round &r,
             std::size_t (*round)(Round &)) {
    const int instructions = counter(PERF_COUNT_HW_INSTRUCTIONS);
    const int cycles = counter(PERF_COUNT_HW_CPU_CYCLES);
    std::vector<double> walls;
    std::vector<std::uint64_t> instrs, cycs;
    std::size_t entries = 0;

    r.work = page;
    round(r);
    for (int i = 0; i < rounds; i++) {
        std::copy(page.begin(), page.end(), r.work.begin());
        ioctl(instructions, PERF_EVENT_IOC_RESET, 0);
        ioctl(cycles, PERF_EVENT_IOC_RESET, 0);
        ioctl(instructions, PERF_EVENT_IOC_ENABLE, 0);
        ioctl(cycles, PERF_EVENT_IOC_ENABLE, 0);
        const auto start = std::chrono::steady_clock::now();
        entries = round(r);
        const auto end = std::chrono::steady_clock::now();
        ioctl(cycles, PERF_EVENT_IOC_DISABLE, 0);
        ioctl(instructions, PERF_EVENT_IOC_DISABLE, 0);
        walls.push_back(std::chrono::duration<double>(end - start).count());
        instrs.push_back(count(instructions));
        cycs.push_back(count(cycles));
    }
    std::sort(walls.begin(), walls.end());
    std::sort(instrs.begin(), instrs.end());
    std::sort(cycs.begin(), cycs.end());
    const double mb = static_cast<double>(page.size()) / (1024.0 * 1024.0);
    const double median = walls[walls.size() / 2], best = walls[0];
    const std::uint64_t instr = instrs[instrs.size() / 2], cyc = cycs[cycs.size() / 2];
    std::printf("%-36s %7.0f MB/s %7.0f MB/s %12llu %7.2f %9llu %6.2f\n", name, mb / median,
                mb / best, static_cast<unsigned long long>(instr),
                static_cast<double>(instr) / static_cast<double>(page.size()),
                static_cast<unsigned long long>(cyc),
                cyc ? static_cast<double>(instr) / static_cast<double>(cyc) : 0.0);
    std::printf("%36s %8.2f ms median, %zu entries\n", "", median * 1e3, entries);
    if (instructions >= 0) close(instructions);
    if (cycles >= 0) close(cycles);
}

} // namespace

int main(int argc, char **argv) {
    if (argc < 2) {
        std::fprintf(stderr, "usage: %s <page.xml> [rounds]\n", argv[0]);
        return 2;
    }
    const int rounds = argc > 2 ? std::atoi(argv[2]) : 60;
    std::ifstream file(argv[1], std::ios::binary);
    if (!file) {
        std::perror(argv[1]);
        return 1;
    }
    const std::vector<std::uint8_t> page((std::istreambuf_iterator<char>(file)),
                                         std::istreambuf_iterator<char>());

    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(0, &set);
    if (sched_setaffinity(0, sizeof set, &set) != 0) {
        std::fprintf(stderr, "not pinned to CPU 0; timings may vary more\n");
    }
    if (counter(PERF_COUNT_HW_INSTRUCTIONS) < 0) {
        std::fprintf(stderr, "hardware counters refused; instructions and cycles read 0\n");
    }

    Round r{
        borink::session("https://acct.blob.core.windows.net", "data", "token"),
        std::vector<std::uint8_t>(page.size()),
        std::vector<borink::ListEntry>(Capacity),
        std::vector<borink::MaybeBytes>(Capacity * 3),
        std::vector<Picked>(Capacity),
        borink::property_set({borink::BlobPropertyCreationTime, borink::BlobPropertyAccessTier,
                              borink::BlobPropertyContentMd5}),
    };

    std::printf("Azure page: %zu bytes, %d rounds, from C++\n\n", page.size(), rounds);
    std::printf("%-36s %10s %10s %12s %7s %9s %6s\n", "", "median", "best", "instructions",
                "per B", "cycles", "IPC");
    measure("borink_fill_listing", page, rounds, r, plain);
    measure("borink_fill_listing_with, three", page, rounds, r, with);
    return 0;
}
