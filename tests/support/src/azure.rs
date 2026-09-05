//! The storage accounts every Azure suite talks to, named once.
//!
//! Two accounts, because a hierarchical namespace answers some requests
//! differently from a flat one and both answers are worth having. They are
//! constants here and not settings, since the recorded corpus carries their
//! names inside the responses: a run against other accounts rewrites every
//! file. The environment overrides them for whoever does want that.
//!
//! Each account has two containers. The live suite writes in one, under a
//! segment named after the run, and a lifecycle rule removes what it leaves.
//! The recorder writes in the other, under fixed names, and empties it
//! itself. The identity each one signs in with may write in its own
//! container and not in the other's, so neither can touch the other's state
//! whatever its code does.
//!
//! Every suite reads the same variables:
//!
//! - `AZURE_STORAGE_ACCESS_TOKEN`: a blob data-plane token. Never optional.
//! - `AZURE_FLAT_ENDPOINT`, `AZURE_HIERARCHICAL_ENDPOINT`: other accounts of
//!   each kind.
//! - `AZURE_LIVE_CONTAINER`, `AZURE_FIXTURES_CONTAINER`: other containers.
//!
//! The live suite runs against one account at a time and adds
//! `AZURE_HIERARCHICAL=1` to pick the hierarchical one. The recorder records
//! from both in one run, because a group of responses holds files from each,
//! and adds `AZURE_STORAGE_ACCESS_TOKEN_ACCOUNT` for its second identity.
//! `docs/AZURE-TESTING.md` says why.

use std::env;

/// The account with a flat namespace.
pub const FLAT_ENDPOINT: &str = "https://borinkstoragetest.blob.core.windows.net";

/// The account with a hierarchical namespace.
pub const HIERARCHICAL_ENDPOINT: &str = "https://borinkstoragehnstest.blob.core.windows.net";

/// The container on each account that the live suite writes in.
pub const LIVE_CONTAINER: &str = "borink-object-test";

/// The container on each account that the recorder writes in.
pub const FIXTURES_CONTAINER: &str = "borink-object-fixtures";

/// Everything the live suite writes lives under this prefix in
/// [`LIVE_CONTAINER`], under a segment named after the run.
pub const LIVE_PREFIX: &str = "borink-object-storage/live/";

/// Everything the recorder writes lives under this prefix in
/// [`FIXTURES_CONTAINER`].
pub const FIXTURES_PREFIX: &str = "borink-object-storage/fixtures/";

/// One storage account, as the suites address it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// `https://account.blob.core.windows.net`, with no path.
    pub endpoint: String,
    /// The container the suite writes in: the live one unless
    /// [`Account::for_fixtures`] said otherwise.
    pub container: String,
    /// Whether the account has a hierarchical namespace.
    pub hierarchical: bool,
}

impl Account {
    /// The flat account, or the one `AZURE_FLAT_ENDPOINT` names.
    pub fn flat() -> Self {
        Self {
            endpoint: env::var("AZURE_FLAT_ENDPOINT").unwrap_or_else(|_| FLAT_ENDPOINT.to_owned()),
            container: container(),
            hierarchical: false,
        }
    }

    /// The hierarchical account, or the one `AZURE_HIERARCHICAL_ENDPOINT` names.
    pub fn hierarchical() -> Self {
        Self {
            endpoint: env::var("AZURE_HIERARCHICAL_ENDPOINT")
                .unwrap_or_else(|_| HIERARCHICAL_ENDPOINT.to_owned()),
            container: container(),
            hierarchical: true,
        }
    }

    /// The account the live suite runs against: the hierarchical one when
    /// `AZURE_HIERARCHICAL` is exactly `1`, the flat one otherwise.
    ///
    /// Only the exact value counts. An exported but empty value must mean a
    /// flat account; otherwise a test that only a hierarchical account can
    /// pass would run against a flat one, and fail for the wrong reason.
    pub fn under_test() -> Self {
        if env::var("AZURE_HIERARCHICAL").is_ok_and(|value| value == "1") {
            Self::hierarchical()
        } else {
            Self::flat()
        }
    }

    /// The same account, addressed at the recorder's container.
    pub fn for_fixtures(self) -> Self {
        Self {
            container: env::var("AZURE_FIXTURES_CONTAINER")
                .unwrap_or_else(|_| FIXTURES_CONTAINER.to_owned()),
            ..self
        }
    }

    /// The account's own name: `borinkstoragetest`.
    pub fn name(&self) -> &str {
        self.endpoint
            .trim_start_matches("https://")
            .split('.')
            .next()
            .unwrap_or(&self.endpoint)
    }
}

fn container() -> String {
    env::var("AZURE_LIVE_CONTAINER").unwrap_or_else(|_| LIVE_CONTAINER.to_owned())
}

/// The data-plane token in `AZURE_STORAGE_ACCESS_TOKEN`.
///
/// Panics when it is not set, with the line that sets it: a suite that got
/// this far was asked to run against the service.
pub fn token() -> String {
    token_from("AZURE_STORAGE_ACCESS_TOKEN")
}

/// The token in the variable `name`, with the same message when it is not set.
pub fn token_from(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} is not set; `tests/azure-setup/token.sh` prints one, see docs/AZURE-TESTING.md"
        )
    })
}
