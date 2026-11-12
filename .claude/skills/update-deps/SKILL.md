---
name: update-deps
description: Moves Flightdeck's pinned versions to their latest stable releases, from the Rust toolchain and mise tools to Polars, both Cargo lockfiles and the GitHub Actions. Runs on direction, since it makes a commit for each kind of pin. Adding a crate the repository does not use yet goes through the dependency decision in docs/recording-research.md instead.
disable-model-invocation: true
---

# Update dependencies

Bring every pin to its latest stable release, which excludes release candidates, betas and
nightlies. Never move a pin backwards. Where a lookup returns a version behind the current
pin, or fails, leave the pin and say so in the report.

Leave three kinds of pin alone and name them in the report. The `gpui` crates come from a
fork of zed whose commits Flightdeck needs, so their `rev` moves only with work on that fork.
A crate requirement written with `=` is held on purpose, so move it only with the Polars
release it follows. The `runs-on` runner images change what CI runs on, so moving one is its
own change.

Run cargo as `mise exec -- cargo`, because the cargo on the shell's path may be another
toolchain than the one `mise.toml` pins. The check is `just check`, `just test-event-capacity`
and `just test-process-capacity`, which together run what CI runs.

Stop before any edit and report the paths when `git status --porcelain` lists anything, since
those are someone's work in progress. Run the check before any edit as well, and stop and
report its output when it fails, since a failure the update did not cause would then be blamed
on it.

Work through the kinds below in order. Finish each with the check passing and a commit that
stages, by path, that kind's pin files and lockfiles and the code its bumps needed, before
starting the next. One file, such as a workflow, can carry pins of two kinds, and staging
takes whole files. A kind whose pins are all at latest makes no commit. When a bump breaks
the check, adapt the code to the new release with the smallest change that makes it pass.
When that would change behavior rather than follow a renamed or deprecated call, revert that
one bump and report why.

## Rust toolchain

`mise upgrade --bump --local rust` moves the pin in `mise.toml`. Confirm the entry kept its
profile and components and that `mise.lock` names the new version, running `mise lock` if it
does not. Write the same version into every `RUSTUP_TOOLCHAIN:` line under
`.github/workflows`, and into the instructions in `README.md` and `desktop/README.md` that
name the old one. Leave a document that records a past measurement under the toolchain it ran
on as it is. A newer clippy usually brings new lints, and their fixes belong in this commit.

## mise tools and mise itself

`mise upgrade --bump --local --exclude rust` moves the other tools in `mise.toml`. Confirm that
`mise.lock` names each new version, as above.

The workflows install just through `jdx/mise-action`, whose `version:` input names a mise
release without its `v` and whose `sha256:` input is the bare digest of that release's
`macos-arm64` binary. Move both together, since the action refuses a mise whose hash differs.
These print them in the form the inputs take:

```
gh api repos/jdx/mise/releases/latest --jq '.tag_name | ltrimstr("v")'
gh api repos/jdx/mise/releases/tags/v<version> --jq '.assets[] | select(.name == "mise-v<version>-macos-arm64") | .digest | ltrimstr("sha256:")'
```

## Polars

Polars is pinned to the commit of a Python Polars release. Find the newest release with
`gh api repos/pola-rs/polars/releases --jq '.[] | select(.prerelease | not) | .tag_name'`,
taking the first `py-` tag, and its commit with
`gh api repos/pola-rs/polars/commits/<tag> --jq .sha`. Where it is newer than the pinned one:

- Set every Polars crate's `rev` in the root `Cargo.toml` to that commit.
- Copy the `[patch.crates-io]` entries of that commit's `Cargo.toml` into the root and the
  desktop `Cargo.toml`, since a patch applies only from the workspace root that declares it.
- Set each `=` requirement that `docs/recording-research.md` says follows Polars to the version
  `mise exec -- cargo tree --invert <crate>` shows Polars using after the move.
- Refresh both lockfiles with `cargo update` for each crate that moved, run once from the root
  and once with `--manifest-path desktop/Cargo.toml`, since every recipe in the check passes
  `--locked`.
- Update the Polars entries of `docs/recording-research.md` to name the new release and its
  revisions, and run the rechecks their Maintenance lines list.

The viewer query measurements in that document were taken on the release they name, so leave
them and report that they predate the move.

## Crates

Run `mise exec -- cargo update --verbose` from the root and again with
`--manifest-path desktop/Cargo.toml`, since each workspace keeps its own lockfile. The lines
marked `Unchanged ... (available: ...)` are releases the version requirements exclude. Where
a member's `Cargo.toml` names one directly without `=`, raise the requirement and adapt the
code. One that only another crate requires stays where that crate holds it.

## GitHub Actions

Each `uses:` under `.github/workflows` pins a commit and names its release in a comment, as
`owner/repo@<sha> # vX.Y.Z`. For each one, take the latest release with
`gh api repos/<owner>/<repo>/releases/latest --jq .tag_name`, resolve the commit it names with
`gh api repos/<owner>/<repo>/commits/<tag> --jq .sha`, and write both. A tool version passed
to an action as an input, such as `tool: cargo-audit@<version>`, moves in the same commit to
that tool's latest release. Read the release notes of each action that moves, for changed
inputs and for what it now does with the tokens its job holds, because no local check runs
the workflows.

## Report

Name each pin that moved, from and to, the commit of each kind, every code change a bump
needed, and every pin left behind with the reason.
