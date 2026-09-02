//! Retired instructions and cycles of the calling thread, read through
//! `perf_event_open`.
//!
//! The kernel refuses this to an unprivileged process when
//! `kernel.perf_event_paranoid` is set high enough, and some distributions
//! set it so. `main.rs` then asks `sudo` to grant `CAP_PERFMON` to the bench
//! binary, which is the least that lets it read its own counters. Counts from
//! the hardware vary by a few hundred instructions between identical runs;
//! callgrind's are exact, see `--callgrind`.
//!
//! `libc` carries the syscall number but not the attribute struct or the
//! constants, so the first version of the struct is written out here. The
//! kernel accepts it with `size` set to that version's 64 bytes.

use std::io;

/// `struct perf_event_attr` as first defined, `PERF_ATTR_SIZE_VER0`.
#[repr(C)]
struct Attr {
    type_: u32,
    size: u32,
    config: u64,
    sample_period: u64,
    sample_type: u64,
    read_format: u64,
    // Bit 0 is `disabled`, bit 5 `exclude_kernel` and bit 6 `exclude_hv`.
    flags: u64,
    wakeup_events: u32,
    bp_type: u32,
    config1: u64,
}

const PERF_TYPE_HARDWARE: u32 = 0;
const PERF_COUNT_HW_CPU_CYCLES: u64 = 0;
const PERF_COUNT_HW_INSTRUCTIONS: u64 = 1;
const DISABLED: u64 = 1 << 0;
const EXCLUDE_KERNEL: u64 = 1 << 5;
const EXCLUDE_HV: u64 = 1 << 6;
// A read then returns the count, the time the counter was enabled and the
// time it was actually running. The two times differ when the kernel could
// not keep the counter on the core the thread ran on.
const PERF_FORMAT_TOTAL_TIME_ENABLED: u64 = 1 << 0;
const PERF_FORMAT_TOTAL_TIME_RUNNING: u64 = 1 << 1;
// `_IO('$', n)`: the type byte `$` shifted by eight, plus the number.
const PERF_EVENT_IOC_ENABLE: libc::c_ulong = 0x2400;
const PERF_EVENT_IOC_DISABLE: libc::c_ulong = 0x2401;
const PERF_EVENT_IOC_RESET: libc::c_ulong = 0x2403;

/// One measurement of both counters.
pub struct Counts {
    pub instructions: u64,
    pub cycles: u64,
    /// Whether both counters ran for the whole measurement.
    pub complete: bool,
}

pub struct Counters {
    instructions: Counter,
    cycles: Counter,
}

struct Counter(libc::c_int);

impl Counters {
    /// Opens both counters, or reports why the kernel refused.
    pub fn open() -> io::Result<Self> {
        Ok(Self {
            instructions: Counter::open(PERF_COUNT_HW_INSTRUCTIONS)?,
            cycles: Counter::open(PERF_COUNT_HW_CPU_CYCLES)?,
        })
    }

    /// Runs `f` and returns what it returned with the counts it took. The two
    /// counters are started one after the other, so each measurement includes
    /// a few dozen instructions of this method's own.
    pub fn measure<T>(&self, f: impl FnOnce() -> T) -> (T, Counts) {
        self.instructions.control(PERF_EVENT_IOC_RESET);
        self.cycles.control(PERF_EVENT_IOC_RESET);
        self.instructions.control(PERF_EVENT_IOC_ENABLE);
        self.cycles.control(PERF_EVENT_IOC_ENABLE);
        let out = f();
        self.cycles.control(PERF_EVENT_IOC_DISABLE);
        self.instructions.control(PERF_EVENT_IOC_DISABLE);
        let (instructions, instructions_complete) = self.instructions.read();
        let (cycles, cycles_complete) = self.cycles.read();
        let counts = Counts {
            instructions,
            cycles,
            complete: instructions_complete && cycles_complete,
        };
        (out, counts)
    }
}

impl Counter {
    fn open(config: u64) -> io::Result<Self> {
        let attr = Attr {
            type_: PERF_TYPE_HARDWARE,
            size: std::mem::size_of::<Attr>() as u32,
            config,
            sample_period: 0,
            sample_type: 0,
            read_format: PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING,
            flags: DISABLED | EXCLUDE_KERNEL | EXCLUDE_HV,
            wakeup_events: 0,
            bp_type: 0,
            config1: 0,
        };
        // SAFETY: the syscall reads `attr`, whose layout is the kernel's, and
        // returns a descriptor or -1. pid 0 and cpu -1 mean this thread, on
        // whichever CPU runs it; group -1 means no group.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                &raw const attr,
                0 as libc::pid_t,
                -1 as libc::c_int,
                -1 as libc::c_int,
                0 as libc::c_ulong,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(fd as libc::c_int))
    }

    fn control(&self, request: libc::c_ulong) {
        // SAFETY: an ioctl on a descriptor this struct owns, with no argument.
        let done = unsafe { libc::ioctl(self.0, request, 0) };
        assert_eq!(done, 0, "controlling a hardware counter");
    }

    /// Returns the count and whether the counter ran the whole time it was
    /// enabled.
    fn read(&self) -> (u64, bool) {
        let mut values = [0u64; 3];
        // SAFETY: with the read format above the kernel writes three `u64`,
        // and `values` is that big.
        let got = unsafe { libc::read(self.0, values.as_mut_ptr().cast(), 24) };
        assert_eq!(got, 24, "reading a hardware counter");
        let [count, enabled, running] = values;
        (count, running == enabled)
    }
}

impl Drop for Counter {
    fn drop(&mut self) {
        // SAFETY: closes a descriptor this struct owns and nothing else uses.
        unsafe { libc::close(self.0) };
    }
}

/// Pins the calling thread to one CPU, so that the counters and the timings
/// come from one core. On a CPU with two kinds of core the counter cannot
/// follow the thread from one kind to the other.
pub fn pin_to_cpu(cpu: usize) -> io::Result<()> {
    // SAFETY: `cpu_set_t` is a plain bit set that starts zeroed, and the
    // macros only set a bit in it.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    unsafe { libc::CPU_SET(cpu, &mut set) };
    // SAFETY: pid 0 is the calling thread, and `set` is a whole `cpu_set_t`.
    let done = unsafe { libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) };
    if done != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
