# Changelog

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
