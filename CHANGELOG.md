# Changelog

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
