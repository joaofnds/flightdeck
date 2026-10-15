# Tiger Style in Flightdeck

Flightdeck follows [Tiger Style][tiger] strictly. Tiger Style is written for Zig, and this
page reads the rules of its Safety, Performance and Developer Experience sections into
Rust. Where the page is silent on a rule of those sections, Tiger's text binds as far as
it applies to Rust. Tiger Style gives the reason for each rule, so read it there, and read
here how the rule applies to Rust and where Flightdeck departs from it.

Hold the code you write or change to every rule here. Bring other code into line only on
a card for that work, never as a side effect of another task. Add a rule's check in the
commit that brings the code into line with it, so the check never fails over code nobody
has fixed yet.

## What the check holds

A reviewer reads each change against every rule below, since a tool holds only some of them.

Rust holds Tiger's snake_case for functions, variables and modules and its braces on every
`if`, and rustfmt holds its layout and four-space indentation. Name every file in
snake_case too, which nothing checks.

## Safety

- Use only simple, explicit control flow, and no recursion. Keep abstractions to the
  few that make the best sense of the domain.
- Put a limit on everything. Give every loop and every queue a fixed upper bound, and
  assert that a loop meant never to end does not end.
- Give every integer that carries a value about the world a type of fixed size, such as
  `i32` for a pid or `u64` for a count of forks or a number the collector stores, and
  never `usize`.
- Keep assertions on in production. Write `assert!`, never `debug_assert!`, and make
  the release profile trap integer overflow and abort on a panic.
- Treat every compiler and clippy warning as an error, clippy's pedantic group included.
- Assert arguments, return values, preconditions, postconditions and invariants wherever a
  bug in this function or a change elsewhere could break them. Never write an assertion that
  restates the statement just above it, such as a guard repeated after it or a field
  compared with the literal it was just set to, unless the condition is critical and
  surprising. An assertion in a `const` block fails the build when a constant or a type
  changes, so it is never one of these.
- Assert only what the code itself guarantees. Handle what a kernel call or another
  program returns as an operating error and never assert it, because under
  `panic = "abort"` an assertion on it turns a change in macOS into a crash.
- Check one condition in each assertion, `assert!(a); assert!(b);` and not
  `assert!(a && b)`. Write an implication as `if a { assert!(b); }`.
- Assert what must not hold as well as what must. Assert a property on two code paths
  where you can, such as before a record is written and after it is read back.
- Assert the relationships of constants, and the sizes of types, in `const` blocks.
- Where a condition is critical and surprising, assert it instead of commenting it.
- Build a precise model of the code first, write it down as assertions, and explain it
  in the code and its comments. A test finds bugs in that model and never proves it
  right.
- Test exhaustively, with valid data, invalid data, and valid data turning invalid.
- Allocate all memory at startup, and never allocate, or free and allocate again,
  after it. Size every buffer once, to a bound read from the system or chosen above the
  largest need measured. This binds the `collector` binary and the library code it runs.
  A test and the code of a binary other than `collector` or the persister may allocate.
  The separate persister, and library code and dependencies used only by persistence,
  may allocate within a fixed working-memory bound for encoding and compressing its
  windows outside the collection loop. All other library code stays startup-allocated,
  whichever binary calls it.
- Handle every error. Report a failed kernel call, and never replace it with a made-up
  value, so an `unwrap_or` or `.ok()` that hides a failure is a defect. Write no
  `unwrap` and no `expect` outside tests, and no `as` conversion anywhere, tests
  included. Discard no value with `let _ =`, `_ =` or `.ok();`, and no `Result` with
  `drop` or with a binding whose name starts with an underscore.
- Declare a variable in the smallest scope, keep few variables in scope, and compute
  or check each one close to its use. Never duplicate a variable or take an alias to it.
- Keep a function to 70 lines, counted from its `fn` line to its closing brace. Give it
  few parameters, a simple return type and a full body. Keep the branches and the state
  in the parent function, push the loops down into helpers, and let helpers compute what
  should change and return it, pure where they can be.
- Keep a line to 100 columns, strings and comments included, in every `.rs` and `.toml`
  file.
- Split a compound condition into nested `if` and `else` branches, and an `else if`
  chain into `else { if }`. Give an `if` its `else` where the other case needs handling
  or an assertion. State an invariant positively, as `index < count`.
- Pass every option a library call takes at the call site, defaults included.
- Run at the program's own pace, and never act directly on an external event. Take in
  what happened since the last step, in one batch with a bound.
- Write no `async`, so a function runs to completion and its preconditions hold
  throughout.
- Read a buffer only up to the length the call that filled it reports, write out no
  bytes past a buffer's data, and zero any padding in what you write out.
- Release a resource the kernel hands out, a CoreFoundation object or Mach memory,
  through a type's `Drop`, and set its acquisition apart with blank lines so a leak
  shows.

## Performance

- Before code that changes what the collector reads or how it moves data, sketch
  network, disk, memory and CPU, by bandwidth and latency, on the card.
- Optimize the slowest resource first, in the order network, disk, memory and CPU, after
  weighting each by how often it is used.
- Keep the control plane apart from the data plane, and batch work on every resource.
  Give the CPU large, predictable chunks of work.
- Depend on the optimizer as little as you can. Extract a hot loop into a function of
  its own that takes plain values rather than `self`.

## Names, shapes and comments

- Find the nouns and verbs that capture what a thing is or does in the domain.
- Never abbreviate a name, except a plain integer in a sort or a matrix calculation, and a
  name that mirrors a header. Where a header declares a type, its fields, a constant or a
  function, name the Rust declaration as the header does, so each can be checked against
  it. Write the type in CamelCase and the fields in snake_case, dropping the prefix they
  all share or keeping it on every field, as `XswUsage` drops it from `xsu_avail`,
  `Timespec` keeps `tv_sec`, and `copyDescription` becomes `copy_description`. Give a
  value its full name where it leaves the declaration, as `received_packets` takes
  `ipackets`.
- Keep an acronym's capitals inside a longer name, as `IOReport` does. Write a name that
  is only an acronym as `Smc`, since clippy refuses `SMC` on a private item.
- Put a name's unit or qualifier last, sorted by significance, as `latency_ms_max`.
- Name a thing for what the reader must know of it, as `arena` tells the reader how its
  memory is freed where `allocator` does not.
- Give related names the same length, as `source` and `target`.
- Start the name of a helper that does part of one function's work, or a callback that
  function passes, with that function's name, as `read_sector` and
  `read_sector_callback`. Pass a closure last.
- Give each name one meaning across the project. Prefer a noun, which reads well in a
  document or a conversation, as `pipeline` over `preparing`.
- Where a function takes two arguments of one type, or arguments that could be swapped,
  take a struct with named fields. Name an `Option` argument so that `None` at the call
  site reads clearly. Pass a singleton, such as a clock, positionally, from the most
  general to the most specific.
- Return the simplest type that works, `()` before `bool`, `bool` before `u64`, `u64`
  before `Option<u64>`, `Option<u64>` before `Result<u64, E>`.
- Pass a type over 16 bytes that a function does not consume by reference. Build a
  large struct in place, allocated at startup and filled through `&mut self`, since Rust
  cannot promise to build a returned value in place. A struct that holds one is then
  built in place too.
- Keep an index, a count and a size apart, in their names too, since a count is the last
  index plus one and a size is a count times its unit. Show how a division rounds, with
  `div_ceil`, with an assertion that it is exact, or with a comment that it floors.
- Put a file's most important item first, `main` first in a binary. Follow a type with
  the types only it uses, then its `impl`. Move a complex nested type to the top level,
  and sort alphabetically where no order is clearly right.
- Comment why the code is the way it is wherever the code alone does not say. Start a
  test with a comment giving its goal and its method. Write a comment as a sentence,
  with a capital letter and a full stop, or a colon where it introduces what follows. A
  comment at the end of a line may be a phrase.
- Write a commit message that says why, since a clone keeps the message and nothing
  else of the conversation.

## Tooling

- Write a tool in Rust, as a binary in this repository, with long flags, and write no
  script. A script is a file whose first line is a shebang, or whose extension is `.sh`,
  `.bash`, `.zsh`, `.py`, `.pl`, `.rb`, `.js`, `.ts`, `.c` or `.m`.

## Departures

- The collector rescans the pid list after every wake that carries a fork, rather than
  at its own pace, because completeness comes first and that misses the fewest
  processes, as [recording-research.md](recording-research.md) measures. A benchmark
  that runs on CI will decide the pace.
- `usize` indexes, counts and sizes things in memory, where Tiger avoids it everywhere,
  because TigerBeetle's own code uses it that way and a ban would put a conversion
  beside every slice index. Keep `usize` or `isize` in a C declaration for a type as wide
  as a pointer, such as `size_t`, `uintptr_t`, `intptr_t` or `CFIndex`, as `Kevent` does
  for its `ident` and its `data`, and convert the value to a type of fixed size where it
  leaves the declaration, as `pid_of` makes the ident an `i32`.
- A macOS call whose answer changes from tick to tick may allocate inside itself, and the
  collector may free what it returns, only where no call found gives that reading without
  root in memory the collector owns, as with `IOReportCreateSamples` and
  `host_processor_info`. Tiger allocates nothing after startup, and the collector departs
  from it here because completeness comes first, as
  [recording-research.md](recording-research.md) records.
- An error that ends the run, or one met while the run ends, may allocate its message,
  where Tiger allocates nothing after startup, because the rule's reasons, steady
  performance and no use after free, no longer apply to a run that is ending. Between
  startup and the run's end, build no message that allocates for an error the collector
  goes on from, and report it as `MachMemory` reports a refused free, with its status in
  `eprintln!` and no `io::Error`.
- Flightdeck sets no number of assertions a function, where Tiger asks for two on average,
  because code held to a count fills its accessors and thin wrappers with assertions that
  cannot fail.
- Markdown may run past 100 columns, since a table row or a link cannot wrap.
- The functions `main` calls are the program's own steps and keep their own names,
  since every function is called from `main` at some depth.
- Take a crate from outside the workspace only under the dependency decision in
  [recording-research.md](recording-research.md#decisions), with its case recorded there,
  where Tiger takes no dependency beyond its toolchain.

[tiger]: https://github.com/tigerbeetle/tigerbeetle/blob/6e39a21fc9d1d912dfbaa1bc7fd8ac108fe379ea/docs/TIGER_STYLE.md
