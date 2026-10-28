# Coding style

Hold the code you write or change to every rule here. Bring other code into line only on a
card for that work, never as a side effect of another task. Add a rule's check in the
commit that brings the code into line with it, so the check never fails over code nobody
has fixed yet.

## Bounds and control flow

- Write no recursion, since its depth, and so its stack, has no bound the code states.
- Put a fixed upper bound on every queue, buffer, retry and loop, so a fault fails fast
  instead of running away. A loop over a collection has the collection's bound, so bound
  the collection. Assert that a loop meant never to end does not end.
- Keep the branches and the state in the parent function, push the loops down into
  helpers, and let a helper compute what should change and return it, pure where it can
  be, so the state changes in one place.
- Give an `if` its `else` where the other case needs handling or an assertion. State an
  invariant positively, as `index < count`, since a negation is easier to get wrong.
- Run the `collector` and `persister` binaries at their own pace, and never act directly
  on an external event. Take in what happened since the last step in one batch with a
  bound, so each step's work stays bounded. Rescan the collector's pid list after every
  wake that carries a fork, although that acts on an external event, because it misses
  the fewest processes, as [recording-research.md](recording-research.md) measures. Other
  binaries may act on events as they arrive, since GPUI or the command line drives them.
- Write no `async` outside the `flightdeck` binary, so a function runs to completion and
  its preconditions hold throughout. In that binary, write `async` only in the futures
  GPUI runs, a task it spawns or a callback that returns a future, and in the `async fn`
  those futures await.

## Assertions

- Assert arguments, return values, preconditions, postconditions and invariants wherever a
  bug in this function or a change elsewhere could break them, so the program stops at the
  first wrong state instead of recording a wrong reading. Never write an assertion that
  restates the statement just above it, such as a guard repeated after it or a field
  compared with the literal it was just set to, unless the condition is critical and
  surprising. An assertion in a `const` block fails the build when a constant or a type
  changes, so it is never one of these.
- Assert only what the code itself guarantees. Handle what a kernel call or another
  program returns as an operating error and never assert it, because release builds keep
  their assertions and abort on a panic, so an assertion on it turns a change in macOS
  into a crash.
- Write `assert!`, never `debug_assert!`, and make every release profile trap integer
  overflow and abort on a panic, so the checks run in the build people use.
- Check one condition in each assertion, `assert!(a); assert!(b);` and not
  `assert!(a && b)`, so a failure names its cause. Write an implication as
  `if a { assert!(b); }`.
- Assert what must not hold as well as what must, and assert a property on two code paths
  where you can, such as before a record is written and after it is read back, since bugs
  gather where valid data meets invalid.
- Assert the relationships of constants, and the sizes of types, in `const` blocks.
- Where a condition is critical and surprising, assert it instead of commenting it.
- Test exhaustively, with valid data, invalid data, and valid data turning invalid.

## Memory

- Allocate the `collector` binary's memory at startup, and never allocate, or free and
  allocate again, after it, so every tick costs the same and no memory is used after it is
  freed. Size every buffer once, to a bound read from the system or chosen above the
  largest need measured. Hold the functions of the `collector` crate that the `collector`
  binary calls to the same rule in every binary and every test that calls them. Those
  functions may allocate in two places, in whichever binary they run:
  - inside a macOS call whose answer changes from tick to tick, and in freeing what it
    returns, only where no call found gives that reading without root in memory the
    collector owns, as with `IOReportCreateSamples` and `host_processor_info`, because
    completeness comes first;
  - in the message of an error that ends the run, or one met while the run ends, since
    neither of the rule's reasons, a steady cost and no use after free, applies to a run
    that is ending.
- Report an error the collector goes on from through `collector::diagnostics::error` with
  `format_args!`, as `MachMemory` reports a refused free, and build no `io::Error` that
  carries a message, so reporting it allocates nothing.
- In the `persister` binary, code outside those `collector` functions may allocate within
  the persister's fixed working-memory bound, which `--memory-bytes` sets, so the
  persister never takes memory the Mac needs.
- In the `flightdeck` binary, code outside those `collector` functions may allocate. Bound
  its memory by the span of readings it holds, never by how long it has run, so that it
  stays flat over a week of use whether its UI window is open or closed.
- In every other binary, and in every test, code outside those `collector` functions may
  allocate.
- Pass a type over 16 bytes that a function does not consume by reference, so a copy
  shows. In the `collector` binary, build a large struct in place, allocated at startup and
  filled through `&mut self`, since Rust cannot promise to build a returned value in place,
  and build a struct that holds one in place too.

## Errors and state

- Handle every error. Report a failed kernel call and never replace it with a made-up
  value, so an `unwrap_or` or `.ok()` that hides a failure is a defect, since a made-up
  reading in the recording is worse than a gap. Discard no `Result` with `drop`, with a
  binding whose name starts with an underscore, or with `.ok();`.
- Keep one copy of each piece of state that can change, since a copy falls out of step
  with the original. Declare a variable in the smallest scope, keep few variables in
  scope, and compute or check each one close to its use, so nothing changes between the
  check and the use.

## Integers and buffers

- Give every integer that carries a value about the world a type of fixed size, such as
  `i32` for a pid or `u64` for a count of forks or a number the collector stores, so a
  recorded value has the same width on every target. Use `usize` only to index, count or
  size things in memory, which Rust's slices index with it. Keep `usize` or `isize` in a C
  declaration for a type as wide as a pointer, such as `size_t` or `CFIndex`, and convert
  the value to a type of fixed size where it leaves the declaration, as `pid_of` makes a
  kevent's ident an `i32`.
- Keep an index, a count and a size apart, in their names too, since a count is the last
  index plus one and a size is a count times its unit. Show how a division rounds, with
  `div_ceil`, with an assertion that it is exact, or with a comment that it floors.
- Read a buffer only up to the length the call that filled it reports, write out no bytes
  past a buffer's data, and zero any padding in what you write out, so no stale memory
  reaches the recording.
- Release a resource the kernel hands out, a CoreFoundation object or Mach memory, through
  a type's `Drop`, so every path through the code releases it.

## Performance

- Before code that changes what the collector reads or how it moves data, sketch network,
  disk, memory and CPU, by bandwidth and latency, on the card, since the largest gains are
  made in the design. Optimize the slowest resource first, in the order network, disk,
  memory and CPU, after weighting each by how often it is used, since a faster resource
  used far more often can cost more.
- Keep the control plane apart from the data plane, batch the work on every resource, and
  give the CPU large, predictable chunks of work, so a fixed cost is paid once a batch.
- Extract a hot loop into a function of its own that takes plain values rather than
  `self`, so neither the optimizer nor the reader has to prove which fields stay the same.

## Names

- Never abbreviate a name, except a plain integer in a sort or a matrix calculation, and a
  name that mirrors a header. Where a header declares a type, its fields, a constant or a
  function, name the Rust declaration as the header does, so each can be checked against
  it. Write the type in CamelCase and the fields in snake_case, dropping the prefix they
  all share or keeping it on every field, as `XswUsage` drops it from `xsu_avail`,
  `Timespec` keeps `tv_sec`, and `copyDescription` becomes `copy_description`. Give a value
  its full name where it leaves the declaration, as `received_packets` takes `ipackets`.
- Keep an acronym's capitals inside a longer name, as `IOReport` does. Write a name that is
  only an acronym as `Smc`, since clippy refuses `SMC` on a private item.
- Put a name's unit or qualifier last, sorted by significance, as `latency_ms_max`, so
  related names sort together.
- Start the name of a helper that does part of one function's work, or a callback that
  function passes, with that function's name, as `read_sector` and `read_sector_callback`,
  so the name shows its caller. Pass a closure last. The functions `main` calls are the
  program's own steps and keep their own names, since every function is called from `main`
  at some depth.
- Give each name one meaning across the project. Prefer a noun, which reads well in a
  document or a conversation, as `pipeline` over `preparing`.
- Name an `Option` argument so that `None` at the call site reads clearly.
- Give a function few parameters, and return the simplest type that works, `()` before
  `bool`, `bool` before `u64`, `u64` before `Option<u64>`, `Option<u64>` before
  `Result<u64, E>`, since each case a return type carries is handled at every call site.
- Name every Rust source file in snake_case.

## Layout and comments

- Put a file's most important item first, `main` first in a binary. Follow a type with the
  types only it uses, then its `impl`. Move a complex nested type to the top level, and
  sort alphabetically where no order is clearly right.
- Comment why the code is the way it is wherever the code alone does not say. Start a test
  with a comment giving its goal and its method, so a reader learns what it proves without
  reading it. Write a comment as a sentence, with a capital letter and a full stop, or a
  colon where it introduces what follows. A comment at the end of a line may be a phrase.

## Tools and dependencies

- Write a tool in Rust, as a binary in this repository, with long flags, and write no
  script, so every tool builds and runs wherever the code does. A script is a file whose
  first line is a shebang, or whose extension is `.sh`, `.bash`, `.zsh`, `.py`, `.pl`,
  `.rb`, `.js`, `.ts`, `.c` or `.m`.
- Take a crate from outside this repository only under the dependency decision in
  [recording-research.md](recording-research.md#decisions), with its case recorded there,
  since each crate adds supply-chain and build cost.
