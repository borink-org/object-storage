// The measurement in ../src/main.rs, made from C through the ABI crate.
//
// Reads the page that `cargo run --release -- --write <path>` wrote, and
// times `borink_fill_listing` and `borink_fill_listing_with` over it: the
// median of the rounds, with instructions and cycles from the hardware
// counters where the kernel grants them. Each round restores the page first,
// outside the measured region, because a read decodes it in place.
//
//     ../src/main.rs --write page.xml
//     ./bench_c page.xml [rounds]

#define _GNU_SOURCE
#include <linux/perf_event.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#include "borink/object_storage.h"

// Enough for the largest page Azure sends, plus its groups of keys.
#define CAPACITY 8192

static int counter(unsigned config) {
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof attr);
    attr.type = PERF_TYPE_HARDWARE;
    attr.size = sizeof attr;
    attr.config = config;
    attr.disabled = 1;
    attr.exclude_kernel = 1;
    attr.exclude_hv = 1;
    return (int)syscall(SYS_perf_event_open, &attr, 0, -1, -1, 0);
}

static uint64_t count(int fd) {
    uint64_t value = 0;
    if (fd >= 0 && read(fd, &value, sizeof value) != (ssize_t)sizeof value) {
        value = 0;
    }
    return value;
}

static int compare_u64(const void *a, const void *b) {
    const uint64_t x = *(const uint64_t *)a, y = *(const uint64_t *)b;
    return x < y ? -1 : x > y;
}

static int compare_double(const void *a, const void *b) {
    const double x = *(const double *)a, y = *(const double *)b;
    return x < y ? -1 : x > y;
}

typedef struct {
    const borink_session *session;
    uint8_t *work;
    size_t len;
    borink_list_entry *entries;
    borink_maybe_bytes *values;
    borink_property_set wanted;
} Round;

static size_t plain(const Round *r) {
    const borink_fill fill = borink_fill_listing(
        r->session, (borink_bytes_mut){r->work, r->len}, r->entries, CAPACITY);
    return fill.filled;
}

// The same read, keeping three properties of every object, which a C
// program reads out of its rows afterwards. The reads are part of the
// measurement, as the Rust bench's entry type is.
static size_t with(const Round *r) {
    const size_t width = borink_property_set_len(r->wanted);
    const borink_fill fill = borink_fill_listing_with(
        r->session, (borink_bytes_mut){r->work, r->len}, r->entries, CAPACITY, r->wanted,
        r->values, CAPACITY * width);
    const size_t created = borink_property_slot(r->wanted, BORINK_BLOB_PROPERTY_CREATION_TIME);
    size_t seen = 0;
    for (size_t i = 0; i < fill.filled; i++) {
        seen += r->values[i * width + created].bytes.len;
    }
    return fill.filled + (seen != 0);
}

static void measure(const char *name, const uint8_t *page, size_t len, int rounds,
                    const Round *r, size_t (*round)(const Round *)) {
    const int instructions = counter(PERF_COUNT_HW_INSTRUCTIONS);
    const int cycles = counter(PERF_COUNT_HW_CPU_CYCLES);
    double *walls = malloc(sizeof(double) * rounds);
    uint64_t *instrs = malloc(sizeof(uint64_t) * rounds);
    uint64_t *cycs = malloc(sizeof(uint64_t) * rounds);
    size_t entries = 0;

    // One warm round, unmeasured.
    memcpy(r->work, page, len);
    round(r);
    for (int i = 0; i < rounds; i++) {
        memcpy(r->work, page, len);
        struct timespec start, end;
        ioctl(instructions, PERF_EVENT_IOC_RESET, 0);
        ioctl(cycles, PERF_EVENT_IOC_RESET, 0);
        ioctl(instructions, PERF_EVENT_IOC_ENABLE, 0);
        ioctl(cycles, PERF_EVENT_IOC_ENABLE, 0);
        clock_gettime(CLOCK_MONOTONIC, &start);
        entries = round(r);
        clock_gettime(CLOCK_MONOTONIC, &end);
        ioctl(cycles, PERF_EVENT_IOC_DISABLE, 0);
        ioctl(instructions, PERF_EVENT_IOC_DISABLE, 0);
        walls[i] = (double)(end.tv_sec - start.tv_sec) + (double)(end.tv_nsec - start.tv_nsec) / 1e9;
        instrs[i] = count(instructions);
        cycs[i] = count(cycles);
    }
    qsort(walls, rounds, sizeof(double), compare_double);
    qsort(instrs, rounds, sizeof(uint64_t), compare_u64);
    qsort(cycs, rounds, sizeof(uint64_t), compare_u64);
    const double mb = (double)len / (1024.0 * 1024.0);
    const double median = walls[rounds / 2], best = walls[0];
    const uint64_t instr = instrs[rounds / 2], cyc = cycs[rounds / 2];
    printf("%-36s %7.0f MB/s %7.0f MB/s %12llu %7.2f %9llu %6.2f\n", name, mb / median, mb / best,
           (unsigned long long)instr, (double)instr / (double)len, (unsigned long long)cyc,
           cyc ? (double)instr / (double)cyc : 0.0);
    printf("%36s %8.2f ms median, %zu entries\n", "", median * 1e3, entries);
    free(walls);
    free(instrs);
    free(cycs);
    if (instructions >= 0) close(instructions);
    if (cycles >= 0) close(cycles);
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <page.xml> [rounds]\n", argv[0]);
        return 2;
    }
    const int rounds = argc > 2 ? atoi(argv[2]) : 60;
    FILE *file = fopen(argv[1], "rb");
    if (!file) {
        perror(argv[1]);
        return 1;
    }
    fseek(file, 0, SEEK_END);
    const size_t len = (size_t)ftell(file);
    fseek(file, 0, SEEK_SET);
    uint8_t *page = malloc(len);
    if (fread(page, 1, len, file) != len) {
        perror("reading the page");
        return 1;
    }
    fclose(file);

    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(0, &set);
    if (sched_setaffinity(0, sizeof set, &set) != 0) {
        fprintf(stderr, "not pinned to CPU 0; timings may vary more\n");
    }
    if (counter(PERF_COUNT_HW_INSTRUCTIONS) < 0) {
        fprintf(stderr, "hardware counters refused; instructions and cycles read 0\n");
    }

    borink_session session;
    session.endpoint = (borink_bytes){(const uint8_t *)"https://acct.blob.core.windows.net", 34};
    session.container = (borink_bytes){(const uint8_t *)"data", 4};
    session.token = (borink_bytes){(const uint8_t *)"token", 5};

    borink_property_set wanted = {0};
    wanted = borink_property_set_with(wanted, BORINK_BLOB_PROPERTY_CREATION_TIME);
    wanted = borink_property_set_with(wanted, BORINK_BLOB_PROPERTY_ACCESS_TIER);
    wanted = borink_property_set_with(wanted, BORINK_BLOB_PROPERTY_CONTENT_MD5);
    Round r = {
        &session,
        malloc(len),
        len,
        calloc(CAPACITY, sizeof(borink_list_entry)),
        calloc(CAPACITY * 3, sizeof(borink_maybe_bytes)),
        wanted,
    };

    printf("Azure page: %zu bytes, %d rounds, from C\n\n", len, rounds);
    printf("%-36s %10s %10s %12s %7s %9s %6s\n", "", "median", "best", "instructions", "per B",
           "cycles", "IPC");
    measure("borink_fill_listing", page, len, rounds, &r, plain);
    measure("borink_fill_listing_with, three", page, len, rounds, &r, with);
    return 0;
}
