# Security policy

FlashRaw parses files that come from the outside world (uploads, email, memory cards), so
hostile input is part of its threat model.

## Supported versions

Only the latest `0.x` release receives security fixes. FlashRaw has not been released yet; until the
first release, the `main` branch is the supported version.

## Reporting a vulnerability

Please report vulnerabilities **privately**, not in a public issue or pull request:

1. Open the repository's **Security** tab on GitHub and choose **Report a vulnerability**
   (a private security advisory that only the maintainers can see).
2. Describe the problem, the version or commit, and how to reproduce it. A minimal file that triggers it is
   the most useful thing you can attach; keep it as small as you can, and do not attach files you are not
   allowed to share.

If the private reporting option is not available on the repository page, open a public issue that says only
that you need a private channel for a security report, without any details, and a maintainer will arrange one.

You will get an acknowledgement and an assessment as soon as a maintainer can look at it. FlashRaw is a small
project: there are no guaranteed response times, but reports are handled before feature work. Once a fix is
ready it is released, the advisory is published with credit to the reporter (if you want it), and the
problem is described in the changelog.

## What counts as a vulnerability

In scope, for any input file or option values:

* a crash, abort, panic or process exit (including allocation failures that abort);
* memory use that is unbounded or far larger than `Limits` and the output size imply;
* hangs and run times out of proportion to the input size (timeouts);
* memory-safety problems in the `unsafe` code (SIMD kernels, the tile writer, the mmap call, the
  self-referential input) and in the C ABI, the Python and WebAssembly bindings and the LibRaw-compatible
  shim, which expose raw pointers;
* output that depends on uninitialised memory, or that escapes the WebAssembly sandbox's contract.

Not in scope: a file that decodes to a wrong picture or fails with a clear error (open a normal bug report or
an unsupported-camera issue), and denial of service by a legitimately huge file within the limits you set.

## What FlashRaw does about it

* Parsing is bounds-checked safe Rust; `unsafe` is limited to a few documented places, each with a
  `SAFETY:` justification (see [docs/architecture.md](docs/architecture.md)).
* Fuzz targets (`fuzz/`) cover the whole open and decode pipeline, the TIFF and BMFF parsers, the bit
  pumps, Huffman, lossless JPEG, packed unpacking, deflate, DNG opcodes and ICC profiles. Every crash
  input is kept in `fuzz/regressions/` and replayed. The fuzz targets and the malformed-file suite run
  locally before releases (`tools/ci/local.sh`); there is no continuous fuzzing service.
* A malformed-file suite truncates and mutates real and synthetic files of every format and requires a
  clean error within a time bound.
* Image dimensions and sample counts are checked against `Limits` before the main buffers are allocated
  ([docs/testing.md](docs/testing.md)). Hardening of the remaining allocation sites is ongoing; an input that
  gets past the limits is exactly the kind of report this policy is for.

## What you should do as a caller

* **Set `Limits` for untrusted files.** The defaults (1.2 billion samples, 65,535 pixels per side) accommodate
  400 MP medium-format sensors and can mean several gigabytes of memory for one decode. For user uploads
  use much smaller values, for example `Limits::default().with_max_samples(200_000_000)` in Rust,
  `max_samples=` in Python (`flashraw.open`), `{ limits: { maxSamples } }` in JavaScript.
* **Bound `threads`** in services: `DecodeOptions::threads = Some(n)` (and the `threads` argument of the
  other bindings) starts `n` worker threads; do not pass a user-controlled value through.
* **Prefer reading the bytes yourself** (`FlashRaw::open(Vec<u8>)`) over memory-mapping files that other
  processes may truncate: a truncated mapped file raises `SIGBUS`.
* **Isolate if the stakes are high.** In browsers and Node.js the WebAssembly package keeps a decoder bug
  inside the sandbox; native callers can run decodes in a separate process.
* Truncated or corrupt sensor data is zero-filled and reported as warnings by default; use the strict
  corruption policy when partial images are unacceptable.

## Safe disclosure

Please give the maintainers a reasonable chance to fix a problem before you publish details, and do not
test against systems or data you do not own. Good-faith research that follows this policy is welcome and
will not be met with legal action.
