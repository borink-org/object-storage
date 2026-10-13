// Tests of every operation on both services at once. They stay inside the
// crate: the check that a layout writes what walking its table writes runs
// only in the crate's own test build.

mod layout;
mod misuse;
