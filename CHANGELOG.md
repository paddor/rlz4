# Changelog

## 0.2.0 (2026-04-12)

### Breaking

- **`RLZ4::Dictionary` wire format changed.** `Dictionary#compress` now
  emits a real LZ4 frame (magic `04 22 4D 18`) with the FLG.DictID bit
  set and `Dict_ID` written into the FrameDescriptor, instead of the
  proprietary `size_le_u32 || lz4_block` blob used in 0.1.x. Output is
  interoperable with the reference `lz4` CLI when both sides use the
  same dictionary file (`lz4 -d -D dict.bin`). Bytes produced by 0.1.x
  cannot be decoded by 0.2.x and vice versa.

### Added

- **`RLZ4::Dictionary#id`** — `u32` derived from `sha256(dict)[0..4]`
  interpreted little-endian. This is the value written into every
  emitted frame's `Dict_ID`, and the value `Dictionary#decompress`
  asserts the incoming frame declares before decoding. A peer that
  encodes against the wrong dictionary now fails fast with a
  `DecompressError` instead of returning corrupt bytes.

### Internal

- Backed by a fork of `lz4_flex` (`frame-dict-support` branch) that
  adds `FrameEncoder::with_dictionary` / `FrameDecoder::with_dictionary`.
  Tracked as a path dependency until upstream merges
  <https://github.com/PSeitz/lz4_flex> PR.

## 0.1.1 (2026-04-08)

### Changed

- **Minimum Ruby version raised to 4.0.** Ractor semantics changed
  significantly in Ruby 4.0, and the test suite no longer passes on
  3.3. Supporting both would mean carrying two shareability stories,
  which is not worth it this early in the gem's life.

### Added

- **`test/test_helper.rb`** — silences `Warning[:experimental]` so
  Ractor tests don't flood stderr, and centralises the `require "rlz4"`
  for future test files.
- **Release workflow** (`.github/workflows/release.yml`) — publishes to
  RubyGems and creates a GitHub release on `v*` tags.
- **README badges** — gem version, license, Ruby version, Rust.

## 0.1.0 (2026-04-08)

Initial release.

- `RLZ4.compress` / `RLZ4.decompress`: LZ4 frame-format compression
  (magic number `04 22 4D 18`), interoperable with any other LZ4 frame
  implementation.
- `RLZ4::Dictionary`: stateful dictionary-based compression using LZ4
  block format with a prepended size. Designed for small messages that
  share a common prefix (e.g. ZMQ messages with a fixed header).
- `RLZ4::DecompressError`: typed exception raised on malformed input.
  Inherits from `StandardError`.
- Ractor-safe: the extension calls `rb_ext_ractor_safe(true)` at load
  time, and all state is either owned or immutable.
- Built on `lz4_flex` with `safe-encode` / `safe-decode` enabled — no
  unsafe decompression on untrusted input.
