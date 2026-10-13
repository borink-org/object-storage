//! Measures the listing reader over one page: the generated Azure page, or a
//! page that `--page` names, read as `--service azure` (the default) or
//! `--service s3` writes it.
//!
//! Each round copies the page into a work buffer (not measured, because the
//! reader decodes in place and a real caller reads each body once), then
//! measures one call: wall time, and retired instructions and cycles where the
//! kernel allows reading the hardware counters.

mod counters;
mod fixture;

use std::hint::black_box;
use std::process::Command;
use std::time::{Duration, Instant};

use borink_object_storage_proto::azure::{BlobProperty, PropertySet, PropertyValues};
use borink_object_storage_proto::s3::{self, ObjectProperty};
use borink_object_storage_proto::{ListEntry, azure};

// Set on the process this one starts after granting itself the capability,
// so that a refusal there is reported rather than asked about again.
const ASKED: &str = "BORINK_BENCH_CAPABILITY_ASKED";

// The service whose listing a page is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Service {
    Azure,
    S3,
}

impl Service {
    const fn flag(self) -> &'static str {
        match self {
            Self::Azure => "azure",
            Self::S3 => "s3",
        }
    }

    // The start tags of the entries of a page, to size the array for it.
    const fn entry_tags(self) -> &'static [&'static [u8]] {
        match self {
            Self::Azure => &[b"<Blob>", b"<BlobPrefix>"],
            Self::S3 => &[
                b"<Contents>",
                b"<Version>",
                b"<DeleteMarker>",
                b"<CommonPrefixes>",
            ],
        }
    }
}

struct Args {
    rounds: Option<usize>,
    entries: usize,
    // A page to read in place of the generated one, and its service.
    page: Option<String>,
    service: Service,
    callgrind: bool,
    // Write the page to this path instead of measuring, for `c/`.
    write: Option<String>,
}

fn args() -> Args {
    let mut args = Args {
        rounds: None,
        entries: 5000,
        page: None,
        service: Service::Azure,
        callgrind: false,
        write: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().expect("a value after the flag");
        match flag.as_str() {
            "--rounds" => args.rounds = Some(value().parse().expect("a number of rounds")),
            "--entries" => args.entries = value().parse().expect("a number of entries"),
            "--page" => args.page = Some(value()),
            "--service" => {
                args.service = match value().as_str() {
                    "azure" => Service::Azure,
                    "s3" => Service::S3,
                    other => panic!("unknown service {other}"),
                }
            }
            "--callgrind" => args.callgrind = true,
            "--write" => args.write = Some(value()),
            other => panic!("unknown flag {other}"),
        }
    }
    args
}

/// Runs this binary under callgrind once per row and reports the
/// instructions per round inside [`measured`], which is exactly what the
/// hardware counters cover. Callgrind counts every instruction, so two runs
/// of the same code give the same number.
fn callgrind(args: &Args) {
    let rounds = args.rounds.unwrap_or(20);
    let exe = std::env::current_exe().expect("the path of this binary");
    println!("instructions per round under callgrind, {rounds} rounds\n");
    {
        let output = Command::new("valgrind")
            .args([
                "--tool=callgrind",
                "--collect-atstart=no",
                "--toggle-collect=*measured*",
                "--callgrind-out-file=/dev/null",
            ])
            .arg(&exe)
            .args(["--rounds", &rounds.to_string()])
            .args(["--entries", &args.entries.to_string()])
            .args(["--service", args.service.flag()])
            .args(args.page.iter().flat_map(|page| ["--page", page.as_str()]))
            // Under valgrind the counters are refused; do not ask about it.
            .env(ASKED, "1")
            .output()
            .expect("valgrind on the path; it is in the dev shell of flake.nix");
        let report = String::from_utf8_lossy(&output.stderr);
        let collected: u64 = report
            .lines()
            .find(|line| line.contains("Collected :"))
            .and_then(|line| line.rsplit(' ').next())
            .unwrap_or_else(|| panic!("no instruction count in callgrind's output:\n{report}"))
            .parse()
            .expect("a number of instructions");
        println!("{:<36} {:>14}", "fill_listing", collected / rounds as u64);
    }
}

/// The region that both the hardware counters and callgrind measure. Not
/// inlined, so that callgrind can toggle collection on its name.
#[inline(never)]
fn measured<T>(f: impl FnOnce() -> T) -> T {
    black_box(f())
}

/// Opens the hardware counters. If the kernel refuses, asks `sudo` to grant
/// this binary `CAP_PERFMON`, the least that lets a process read its own
/// counters, and runs the same command again. The grant is a file
/// capability, so it lasts until the next build replaces the binary and
/// changes nothing on the system.
fn counters() -> Option<counters::Counters> {
    let error = match counters::Counters::open() {
        Ok(counters) => return Some(counters),
        Err(error) => error,
    };
    if std::env::var_os(ASKED).is_none() {
        let exe = std::env::current_exe().expect("the path of this binary");
        eprintln!("hardware counters refused ({error}); granting the capability with");
        eprintln!("    sudo setcap cap_perfmon+ep {}", exe.display());
        let granted = Command::new("sudo")
            .args(["setcap", "cap_perfmon+ep"])
            .arg(&exe)
            .status()
            .is_ok_and(|status| status.success());
        if granted {
            let status = Command::new(&exe)
                .args(std::env::args().skip(1))
                .env(ASKED, "1")
                .status()
                .expect("running this binary again");
            std::process::exit(status.code().unwrap_or(1));
        }
        eprintln!("the capability was not granted; wall time only");
    } else {
        eprintln!("hardware counters refused ({error}); wall time only");
    }
    None
}

struct Sample {
    wall: Duration,
    counts: Option<counters::Counts>,
    entries: usize,
}

fn main() {
    let args = args();
    if let Some(path) = &args.write {
        std::fs::write(path, fixture::azure(args.entries)).expect("writing the page");
        return;
    }
    if args.callgrind {
        return callgrind(&args);
    }
    if let Err(error) = counters::pin_to_cpu(0) {
        eprintln!("not pinned to CPU 0 ({error}); timings may vary more");
    }
    let rounds = args.rounds.unwrap_or(60);
    let counters = counters();
    let page = match &args.page {
        Some(path) => std::fs::read(path).expect("reading the page"),
        None => fixture::azure(args.entries),
    };
    // The array holds every entry of the page, and no more: it is allocated
    // in each round.
    let room: usize = args
        .service
        .entry_tags()
        .iter()
        .map(|tag| {
            page.windows(tag.len())
                .filter(|window| window == tag)
                .count()
        })
        .sum();
    println!(
        "{} page {}: {} bytes, {room} entries, {rounds} rounds\n",
        args.service.flag(),
        args.page.as_deref().unwrap_or("generated"),
        page.len(),
    );
    println!(
        "{:<36} {:>10} {:>10} {:>12} {:>7} {:>9} {:>6}",
        "", "median", "best", "instructions", "per B", "cycles", "IPC"
    );

    match args.service {
        Service::Azure => azure_rows(&page, room, rounds, counters.as_ref()),
        Service::S3 => s3_rows(&page, room, rounds, counters.as_ref()),
    }
}

fn azure_rows(page: &[u8], room: usize, rounds: usize, counters: Option<&counters::Counters>) {
    measure(
        "fill_listing",
        page,
        rounds,
        counters,
        |_| (),
        |work, ()| {
            let mut entries = vec![ListEntry::default(); room];
            let filled = azure::fill_listing(work, &mut entries).unwrap().filled;
            black_box(&entries);
            filled
        },
    );
    // The same read through the C entry point, in this binary, so that the
    // cost of the boundary is measured apart from how the archive is built.
    // `c/` measures it from a C program.
    //
    // Measured on 2026-10-13: the boundary costs nothing this bench can see,
    // and the plain read runs the same instructions whether this binary
    // links the C crate or not. It once ran 12% more with the crate linked.
    measure(
        "borink_fill_listing, from Rust",
        page,
        rounds,
        counters,
        |_| (),
        |work, ()| {
            let mut entries = vec![borink_object_storage_c::ListEntry::default(); room];
            // SAFETY: the body and the array are live for the call, and
            // nothing else reaches them.
            let fill = unsafe {
                borink_object_storage_c::borink_fill_listing(
                    borink_object_storage_c::BytesMut {
                        ptr: work.as_mut_ptr(),
                        len: work.len(),
                    },
                    entries.as_mut_ptr(),
                    entries.len(),
                )
            };
            black_box(&entries);
            fill.filled
        },
    );
    // The same read, keeping three properties of every object, into an
    // entry type of the caller's own.
    const THREE: PropertySet = PropertySet::of(&[
        BlobProperty::CreationTime,
        BlobProperty::AccessTier,
        BlobProperty::ContentMd5,
    ]);
    measure(
        "fill_listing_with, three properties",
        page,
        rounds,
        counters,
        |_| (),
        |work, ()| {
            let mut entries = vec![Picked::default(); room];
            let filled = azure::fill_listing_with(work, &mut entries, THREE, Picked::build)
                .unwrap()
                .filled;
            black_box(&entries);
            filled
        },
    );
    // The same read, keeping every property, as a caller who wants all that
    // Azure wrote asks for it.
    const EVERY: PropertySet = PropertySet::of(BlobProperty::ALL);
    measure(
        "fill_listing_with, every property",
        page,
        rounds,
        counters,
        |_| (),
        |work, ()| {
            let mut entries = vec![Every::default(); room];
            let filled = azure::fill_listing_with(work, &mut entries, EVERY, |entry, values| {
                Every::of(entry, values.all())
            })
            .unwrap()
            .filled;
            black_box(&entries);
            filled
        },
    );
}

fn s3_rows(page: &[u8], room: usize, rounds: usize, counters: Option<&counters::Counters>) {
    measure(
        "s3 fill_listing",
        page,
        rounds,
        counters,
        |_| (),
        |work, ()| {
            let mut entries = vec![ListEntry::default(); room];
            let filled = s3::fill_listing(work, &mut entries).unwrap().filled;
            black_box(&entries);
            filled
        },
    );
    // The same read, keeping two properties of every object, into an entry
    // type of the caller's own.
    const TWO: s3::PropertySet = s3::PropertySet::of(&[
        ObjectProperty::StorageClass,
        ObjectProperty::ChecksumAlgorithm,
    ]);
    measure(
        "s3 fill_listing_with, two properties",
        page,
        rounds,
        counters,
        |_| (),
        |work, ()| {
            let mut entries = vec![S3Picked::default(); room];
            let filled = s3::fill_listing_with(work, &mut entries, TWO, S3Picked::build)
                .unwrap()
                .filled;
            black_box(&entries);
            filled
        },
    );
    // The same read, keeping every property.
    const EVERY: s3::PropertySet = s3::PropertySet::of(ObjectProperty::ALL);
    measure(
        "s3 fill_listing_with, every property",
        page,
        rounds,
        counters,
        |_| (),
        |work, ()| {
            let mut entries = vec![Every::default(); room];
            let filled = s3::fill_listing_with(work, &mut entries, EVERY, |entry, values| {
                Every::of(entry, values.all())
            })
            .unwrap()
            .filled;
            black_box(&entries);
            filled
        },
    );
}

// The fields are only read through `black_box`, which the compiler does not
// count.
#[allow(dead_code)]
#[derive(Clone, Copy, Default)]
struct Picked<'b> {
    key: &'b str,
    size: Option<u64>,
    created: Option<&'b [u8]>,
    tier: Option<&'b [u8]>,
    md5: Option<&'b [u8]>,
}

impl<'b> Picked<'b> {
    fn build(entry: ListEntry<'b>, values: PropertyValues<'_, 'b>) -> Self {
        Self {
            key: entry.key,
            size: entry.size,
            created: values.get(BlobProperty::CreationTime),
            tier: values.get(BlobProperty::AccessTier),
            md5: values.get(BlobProperty::ContentMd5),
        }
    }
}

// The fields are only read through `black_box`, which the compiler does not
// count.
#[allow(dead_code)]
#[derive(Clone, Copy, Default)]
struct S3Picked<'b> {
    key: &'b str,
    size: Option<u64>,
    class: Option<&'b [u8]>,
    checksum: Option<&'b [u8]>,
}

impl<'b> S3Picked<'b> {
    fn build(entry: ListEntry<'b>, values: s3::PropertyValues<'_, 'b>) -> Self {
        Self {
            key: entry.key,
            size: entry.size,
            class: values.get(ObjectProperty::StorageClass),
            checksum: values.get(ObjectProperty::ChecksumAlgorithm),
        }
    }
}

// An entry and how many of the wanted properties it gave a value for. The
// fields are only read through `black_box`.
#[allow(dead_code)]
#[derive(Clone, Copy, Default)]
struct Every<'b> {
    key: &'b str,
    size: Option<u64>,
    present: usize,
}

impl<'b> Every<'b> {
    fn of(entry: ListEntry<'b>, values: &[Option<&'b [u8]>]) -> Self {
        Self {
            key: entry.key,
            size: entry.size,
            present: values.iter().filter(|value| value.is_some()).count(),
        }
    }
}

// Runs `round` `rounds` times over a fresh copy of the page and reports the
// samples. `prepare` runs on the copy before each measurement and hands
// `round` what it made, so a row can measure part of a read.
fn measure<P>(
    name: &str,
    page: &[u8],
    rounds: usize,
    counters: Option<&counters::Counters>,
    mut prepare: impl FnMut(&mut [u8]) -> P,
    mut round: impl FnMut(&mut [u8], P) -> usize,
) {
    let mut work = vec![0u8; page.len()];
    let mut samples = Vec::with_capacity(rounds);
    // One round to warm the caches and the page tables of the work buffer,
    // outside `measured` so that callgrind does not count it.
    work.copy_from_slice(page);
    let prepared = prepare(&mut work);
    round(&mut work, prepared);
    for _ in 0..rounds {
        work.copy_from_slice(page);
        let prepared = prepare(&mut work);
        let timed = || {
            let at = Instant::now();
            let entries = measured(|| round(&mut work, prepared));
            (at.elapsed(), entries)
        };
        let sample = match counters {
            Some(counters) => {
                let ((wall, entries), counts) = counters.measure(timed);
                Sample {
                    wall,
                    counts: Some(counts),
                    entries,
                }
            }
            None => {
                let (wall, entries) = timed();
                Sample {
                    wall,
                    counts: None,
                    entries,
                }
            }
        };
        samples.push(sample);
    }

    let mut walls: Vec<f64> = samples.iter().map(|s| s.wall.as_secs_f64()).collect();
    walls.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mb = page.len() as f64 / (1024.0 * 1024.0);
    let median = walls[walls.len() / 2];
    let best = walls[0];
    // A round the counter did not cover completely says nothing about the
    // code, so it is left out of the medians and counted instead.
    let complete: Vec<&counters::Counts> = samples
        .iter()
        .filter_map(|s| s.counts.as_ref())
        .filter(|c| c.complete)
        .collect();
    let incomplete = samples.iter().filter(|s| s.counts.is_some()).count() - complete.len();
    let instructions = median_of(complete.iter().map(|c| c.instructions));
    let cycles = median_of(complete.iter().map(|c| c.cycles));
    let entries = samples[0].entries;

    print!(
        "{name:<36} {:>5.0} MB/s {:>5.0} MB/s",
        mb / median,
        mb / best
    );
    match (instructions, cycles) {
        (Some(instructions), Some(cycles)) => println!(
            " {instructions:>12} {:>7.2} {cycles:>9} {:>6.2}",
            instructions as f64 / page.len() as f64,
            instructions as f64 / cycles as f64
        ),
        _ => println!(),
    }
    print!(
        "{:<36} {:>7.2} ms median, {entries} entries",
        "",
        median * 1000.0
    );
    if incomplete > 0 {
        print!(", {incomplete} rounds not fully counted and left out");
    }
    println!();
}

fn median_of(values: impl Iterator<Item = u64>) -> Option<u64> {
    let mut values: Vec<u64> = values.collect();
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}
