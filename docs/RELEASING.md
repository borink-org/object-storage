# Releasing

`borink-object-storage-proto` and `borink-object-storage-crypto` are released together, at one version, with [touchgate](https://github.com/borink-org/touchgate). Its README describes the process and what it guarantees; this file records how this repository is set up for it.

## A release

1. Start the `touchgate` workflow on `master` with the version, such as `0.0.4`. It dates the `## Unreleased` section of `CHANGELOG.md`, sets the version on both crates and on crypto's requirement on proto, and opens a pull request from `release/<version>`.
2. Check out that branch, look it over, and approve it with the release key, which asks for its PIN and a touch. An approval lasts an hour, so merge once the checks pass, or approve again.

   ```nu
   git fetch origin
   git switch release/0.0.4
   touchgate approve
   ```

   `touchgate approve` signs the empty `Approve release 0.0.4.` commit and pushes it, without changing git's signing settings. Install it with `cargo install --locked --git https://github.com/borink-org/touchgate`.

3. Merge the pull request with a merge commit. The `touchgate` workflow then runs `release.yml` from the `publish` branch, which checks the approval, publishes proto and then crypto to crates.io, and only then tags `v<version>` and makes the GitHub release.

Every pull request that changes either crate adds a line under `## Unreleased`, or carries the `no-changelog` label. The `touchgate` workflow checks both, and that the newest released section of the changelog is the crates' version.

## Settings

The release is only as safe as these settings. They are made by `borink-infra`, the organization's owner, whose credentials no tool or agent holds.

- `tiptenbrink` has the Maintain role on this repository, not Admin.
- The `publish` branch holds the copy of `release.yml` that runs. A ruleset restricts its creation, updates and deletion, and blocks force pushes, with only organization administrators allowed to bypass it. `master` needs a `release.yml` too, because GitHub starts a workflow by hand only if the default branch has a file at that path, even when it then runs the file from another branch. Only the copy on `publish` ever releases: run from `master`, the publish job is refused by the `release` environment, so whatever the `master` copy says, it cannot publish. Keep it the same as the `publish` copy so that `master` shows what runs: when `borink-infra` changes `publish`, make the same change on `master` in a pull request.
- The `release` environment admits the `publish` branch alone, by its exact name.
- Immutable releases are on, GitHub Actions may create pull requests, and merge commits are allowed.
- On crates.io, both crates trust only the workflow `release.yml` of `borink-org/object-storage` in the environment `release`, with Trusted Publishing only turned on and no API tokens.

`release.yml` calls touchgate's release workflow at a pinned commit, with the release key. Changing either is a change to the `publish` branch, so `borink-infra` makes it, after reading what a new touchgate commit changes.
