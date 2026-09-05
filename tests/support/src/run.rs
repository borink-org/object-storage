//! The name of one run of a live suite, which every key it writes sits under.
//!
//! Two runs against the same account must not see each other: two pull
//! requests approved together, a workstation run beside a CI run, or a run
//! that was killed halfway and the one that replaced it. So a run writes
//! under a segment of its own and never cleans up after itself; a lifecycle
//! rule on the account removes what runs leave behind. That holds for any
//! object store, which is why this knows nothing about which one.
//!
//! `TEST_RUN` names the run when it is set. The run scripts set it from the
//! CI run identifier or from the clock, so the two accounts of one run share
//! a name and a run can be found in the account afterwards. A process that is
//! not given one makes one up, once.

use std::env;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// The run's name: a single path segment, safe in any key.
pub fn id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| match env::var("TEST_RUN") {
        Ok(id) if !id.is_empty() && id.bytes().all(is_plain) => id,
        Ok(id) => panic!("TEST_RUN={id:?} is not a plain path segment"),
        Err(_) => {
            let unix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("the clock is after 1970")
                .as_secs();
            format!("local-{unix}-{}", std::process::id())
        }
    })
}

fn is_plain(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_run_made_up_here_is_one_plain_segment_and_stays_the_same() {
        let id = super::id();
        assert!(id.starts_with("local-"));
        assert!(id.bytes().all(super::is_plain));
        assert_eq!(super::id(), id);
    }
}
