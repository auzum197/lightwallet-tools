# darkside-decl

The declaration parser for the darkside: an authored text file
turned into `darkside-chain` values plus scenario scripts.

The file is authoritative and the parser drives the chain API. There is no
serialize path. Declarations are literal by design so an external harness
can derive ground truth by inspection, which is why the format has no
randomization construct (the Rust API does).

A malformed declaration fails at parse time: non-observable barriers,
forks above a parent's tip, undeclared accounts, unknown corruption words,
inconsistent activation heights, and overdrawn sends (the last surfaces
while the world is built, before anything is served).
