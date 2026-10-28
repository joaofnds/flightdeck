@GLOSSARY.md

Before you write or review code in this repository, read docs/coding-style.md in that
turn. It outranks ~/.agents/rulebook/coding-style, ~/.agents/rulebook/engineering.md and
~/.agents/rulebook/testing wherever they disagree, and the hard lines of
~/.claude/CLAUDE.md outrank it.

Put each check where `.github/workflows/check.yml` runs it. Add nothing that runs on a
contributor's machine without their asking, such as a git hook or a `rust-toolchain.toml`,
which makes rustup install that toolchain, since what runs there is theirs to choose.
