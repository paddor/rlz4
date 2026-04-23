# rlz4

[![Gem Version](https://img.shields.io/gem/v/rlz4?color=e9573f)](https://rubygems.org/gems/rlz4)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Ruby](https://img.shields.io/badge/Ruby-%3E%3D%204.0-CC342D?logo=ruby&logoColor=white)](https://www.ruby-lang.org)
[![Rust](https://img.shields.io/badge/Rust-stable-dea584?logo=rust&logoColor=white)](https://www.rust-lang.org)

Ractor-safe LZ4 bindings for Ruby, built as a Rust extension on top of
[`lz4_flex`](https://github.com/PSeitz/lz4_flex) via [`magnus`](https://github.com/matsadler/magnus).

## Why?

The existing Ruby LZ4 gems are broken under Ractor:

- [`lz4-ruby`](https://github.com/komiya-atsushi/lz4-ruby)
- [`lz4-flex-rb`](https://github.com/Shopify/lz4-flex-rb)

`rlz4` marks the extension Ractor-safe at load time and uses only owned,
thread-safe state, so it can be called from any Ractor.

## Install

```ruby
# Gemfile
gem "rlz4"
```

Building requires a Rust toolchain (stable).

## API

Three classes plus one utility module function:

| | Purpose | Wire format |
|---|---|---|
| `RLZ4::Dictionary` | Value type: dict bytes + 4-byte id | — |
| `RLZ4::FrameCodec` | Optionally dict-bound frame codec | LZ4 frame (`04 22 4D 18`), interoperable with `lz4` CLI |
| `RLZ4::BlockCodec` | Optionally dict-bound block codec, reusable scratch | Raw LZ4 block, no framing |
| `RLZ4.compress_bound(n)` | Worst-case output size for input size `n` | — |

Invalid input on decompress raises `RLZ4::DecompressError`
(a `StandardError` subclass).

## RLZ4::Dictionary

Pure value type — just the dict bytes plus a 4-byte id. Built on
`Data.define`, so it's immutable, has value equality, and is
shareable across `Ractor`s. The id defaults to `sha256(bytes)[0, 4]`
interpreted little-endian (the derivation LZ4 frame `FLG.DictID`
uses); override with `id:` if you need a coordinated value.

```ruby
dict = RLZ4::Dictionary.new(bytes: "schema=v1 type=message field1=")
dict.bytes  # => "schema=v1..." frozen binary
dict.id     # => u32
dict.size   # => 30

# With a caller-supplied id (e.g. from an out-of-band protocol):
custom = RLZ4::Dictionary.new(bytes: raw, id: 0xDEAD_BEEF)
```

## RLZ4::FrameCodec — frame-format LZ4

Emits a real LZ4 frame (magic `04 22 4D 18`), interoperable with the
`lz4` CLI. With a dictionary, sets `FLG.DictID` and writes `Dict_ID`
into the FrameDescriptor — a receiver routing by id can pick the
right dict from a set purely by parsing the frame header.

Stateless (no scratch), so `FrameCodec` instances are shareable
across `Ractor`s.

```ruby
codec = RLZ4::FrameCodec.new                           # no dict
codec = RLZ4::FrameCodec.new(dict: dict)               # Dictionary value
codec = RLZ4::FrameCodec.new(dict: "raw bytes here")   # String shortcut

ct = codec.compress("hello world" * 100)
pt = codec.decompress(ct)

codec.has_dict?  # => true / false
codec.id         # => u32 id when dict-bound, nil otherwise
codec.size       # => dict size when dict-bound, 0 otherwise
```

Dict id mismatch on decompress raises `RLZ4::DecompressError`
before touching the payload — no silently corrupt output.

## RLZ4::BlockCodec — block-format LZ4

For hot paths that compress many small messages and want to amortise
allocation. Emits a raw LZ4 block — no frame header, no end-mark,
no checksum. Not interoperable with the reference `lz4` CLI; meant
for callers who carry their own framing (e.g. ZMTP transports).

Wraps a reusable 16 KiB scratch hash table. With a dictionary, also
carries a pristine dict-loaded table and restores it into the scratch
via a single 16 KiB `memcpy` before each compress call — so dict
initialisation is paid once at construction, not per call.

```ruby
codec = RLZ4::BlockCodec.new                           # no dict
codec = RLZ4::BlockCodec.new(dict: dict)               # Dictionary value
codec = RLZ4::BlockCodec.new(dict: "raw bytes here")   # String shortcut

ct = codec.compress("hello world" * 100)
pt = codec.decompress(ct, decompressed_size: 1100)
```

`#decompress` requires `decompressed_size:` because raw LZ4 blocks
carry no length prefix. The decoder refuses to write past that
value even on crafted malformed input — raises
`RLZ4::DecompressError` on any overrun.

Use `RLZ4.compress_bound(n)` to pre-size output buffers.

`BlockCodec` holds a `RefCell` internally and is **thread-local** —
do not cross `Ractor` boundaries. Allocate one per `Ractor`. The
block format has no on-wire `Dict_ID` field; a dict mismatch
produces garbage plaintext (not an error). Detect at a higher
layer (checksum, schema validation, etc.).

## Ractor safety

`Dictionary` and `FrameCodec` can be used from any `Ractor`. Example:

```ruby
ractors = 4.times.map do |i|
  Ractor.new(i) do |idx|
    codec = RLZ4::FrameCodec.new
    pt    = "ractor #{idx} payload " * 1000
    1000.times do
      ct = codec.compress(pt)
      raise "mismatch" unless codec.decompress(ct) == pt
    end
    :ok
  end
end
ractors.map(&:value) # => [:ok, :ok, :ok, :ok]
```

`BlockCodec` must not cross `Ractor` boundaries — allocate one per
`Ractor`.

## Non-goals

- High-compression mode (LZ4_HC).
- Streaming / chunked compression.
- Preservation of string encoding on decompress (output is always binary).
- Dictionary training from a sample corpus. LZ4 has no equivalent of
  Zstd's `ZDICT_trainFromBuffer`. Dictionaries are caller-supplied
  raw bytes.

## License

MIT
