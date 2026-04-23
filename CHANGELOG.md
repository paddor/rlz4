# Changelog

## 0.3.0 (2026-04-23)

### Added

- **`RLZ4::BlockCodec`** — reusable scratch for LZ4 block-format
  compression. Wraps lz4_flex's `CompressTable` (16 KiB hash table).
  - `BlockCodec.new` — no-dict codec with a single scratch table.
  - `BlockCodec.new(dict: bytes)` — dict is hashed into a pristine
    table exactly once at construction; every `#compress` call restores
    the pristine state into a scratch table with a single 16 KiB
    `memcpy`, amortising dict initialisation across the codec's lifetime.
    Dict is a permanent property of the codec; construct a fresh codec
    to change dicts.
  - `#compress(bytes)` — block-format compress. Output is the raw LZ4
    block: no magic, no frame header, no end-mark, no checksum.
  - `#decompress(bytes, decompressed_size:)` — bounded block decompress.
    Refuses to write past `decompressed_size` even on crafted malformed
    input; raises `RLZ4::DecompressError` on any failure. The decoder
    consults no scratch (LZ4 block decoding is stateless); the method
    lives on `BlockCodec` for API symmetry with `#compress` and to give
    callers one object per worker.
  - `#has_dict?` — `true` iff constructed with `dict:`.
  - `#size` — approximate memory footprint (16 KiB, or 32 KiB + dict
    bytes for a dict codec).
  - `BlockCodec` is thread-local by construction (internal `RefCell`).
    Must not cross `Ractor` boundaries — allocate a new one per `Ractor`.
- **`RLZ4.compress_bound(size)`** — exposes
  `lz4_flex::block::get_maximum_output_size` so callers can pre-size
  output buffers without guessing.

### Changed

- **Frame-format module functions renamed** to `compress_frame` /
  `decompress_frame`. The 0.1/0.2 names (`compress`, `decompress`) are
  kept as aliases for one release cycle and will be removed in 0.4.
  This makes room for block-format primitives on the module surface
  without collisions.

### Performance

- **Zero-copy input path.** Compress and decompress no longer copy the
  input `String`'s bytes into an owned `Vec<u8>` before handing them to
  lz4_flex; they borrow the `RString` slice directly. Saves one
  input-sized `memcpy` per call. Safe because the compress/decompress
  code paths make no Ruby callbacks and trigger no Ruby allocations
  while the borrow is live. Measured impact on 1 KiB round-trip (YJIT):
  0.3–0.4 µs saved on repetitive and text-like inputs (~25%
  reduction); negligible on random input where compression cost
  dominates.

### Internal

- Fork of `lz4_flex` extends the block-format API for `BlockCodec`:
  - `block::compress_into_with_table_and_dict` — one-shot compress with
    a cleared table + re-init of the dict on every call.
  - `block::compress_into_with_loaded_table_and_dict` — compress with
    a table the caller has already populated; used with
    `CompressTable::copy_from` to amortise dict init across calls.
  - `CompressTable::load_dict` / `CompressTable::copy_from` — populate
    a table with dict positions once, restore via `memcpy` per compress.
  - `HashTable4K::copy_from` / `HashTable4KU16::copy_from` — reuses
    the table's existing allocation (no heap traffic).
  Plus the existing `FrameEncoder::with_dictionary` /
  `FrameDecoder::with_dictionary` constructors. Tracked on the
  `frame-dict-support` branch.

## 0.2.1 (2026-04-15)

### Docs

- **README: corrected the `RLZ4::Dictionary` section** that still
  claimed dict compression used "LZ4 block format with the original
  size prepended". Since 0.2.0, `Dictionary#compress` has actually
  emitted a real LZ4 frame with the `FLG.DictID` bit set and
  `Dict_ID` written into the FrameDescriptor — the README just
  never caught up.
- **New "Dictionary IDs" section** explaining that the `Dict_ID`
  `Dictionary#id` computes is the same one that rides along in
  every emitted frame's FrameDescriptor, so receivers with multiple
  dictionaries can route incoming frames purely by parsing the
  frame header. Also documents that dict training from samples is
  not supported (LZ4 has no ZDICT equivalent).

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
