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

## Usage

### Frame format (module functions)

```ruby
require "rlz4"

compressed   = RLZ4.compress_frame("hello world" * 100)
decompressed = RLZ4.decompress_frame(compressed)

# Wire format is standard LZ4 frame (magic number 04 22 4D 18),
# interoperable with any other LZ4 frame implementation.
```

`RLZ4.compress` / `RLZ4.decompress` are kept as aliases for
`compress_frame` / `decompress_frame` and will be removed in 0.4.

Invalid input raises `RLZ4::DecompressError` (a `StandardError` subclass):

```ruby
begin
  RLZ4.decompress_frame("not a valid lz4 frame")
rescue RLZ4::DecompressError => e
  warn e.message
end
```

### Block format (stateful codec)

For hot paths that compress many small messages and want to amortise
allocation, use `RLZ4::BlockCodec`. It wraps a reusable scratch hash
table and emits raw LZ4 blocks — no frame header, no end-mark, no
checksum. Not interoperable with the reference `lz4` CLI, meant for
callers who carry their own framing (e.g. ZMTP transports).

```ruby
codec = RLZ4::BlockCodec.new

msg = "small message " * 8
ct  = codec.compress(msg)
pt  = codec.decompress(ct, decompressed_size: msg.bytesize)
```

For a **shared dictionary**, pass it at construction time. The dict
is hashed into a pristine table exactly once; every subsequent
`#compress` call restores the pristine state via a 16 KiB `memcpy`.
This amortises dict initialisation across the codec's lifetime
instead of paying ~3–5 µs per call to re-hash the dict.

```ruby
codec = RLZ4::BlockCodec.new(dict: "common log prefix: ")
ct = codec.compress("common log prefix: event=login user=alice")
pt = codec.decompress(ct, decompressed_size: ct.bytesize)
```

The dict is a permanent property of the codec. To change dicts, build
a fresh codec. The peer on the other end must construct its codec
with the same dict bytes.

`#decompress` requires `decompressed_size:` because raw LZ4 blocks
carry no length prefix, and uses it as a hard upper bound on output
size. Crafted inputs that try to write more than `decompressed_size`
raise `RLZ4::DecompressError`.

Use `RLZ4.compress_bound(n)` to pre-size output buffers.

`BlockCodec` is thread-local — **do not cross `Ractor` boundaries**.
Allocate one codec per `Ractor`.

### Dictionary compression

For workloads where many small messages share a common prefix (e.g. ZMQ
messages with a fixed header), a shared dictionary massively improves the
compression ratio. `RLZ4::Dictionary#compress` emits a **real LZ4 frame**
(magic `04 22 4D 18`) with the `FLG.DictID` bit set and the dictionary's
`Dict_ID` written into the FrameDescriptor — interoperable with the
reference `lz4` CLI given the same dictionary file (`lz4 -d -D dict.bin`).

```ruby
dict = RLZ4::Dictionary.new("schema=v1 type=message field1=")

compressed   = dict.compress("schema=v1 type=message field1=payload")
decompressed = dict.decompress(compressed)

dict.size  # => 30
dict.id    # => u32 Dict_ID
```

`RLZ4::Dictionary` is immutable after construction and can be shared across
Ractors.

## Dictionary IDs

`Dictionary#id` is a `u32` derived from `sha256(dict_bytes)[0..4]`
interpreted little-endian. The LZ4 frame spec defines `Dict_ID` as
an application-defined field with no reserved ranges and no central
registrar, so the full `u32` space is usable.

The id **is on the wire**: `Dictionary#compress` sets `FLG.DictID = 1`
and writes the id into the FrameDescriptor. On decode, `rlz4` parses
the incoming frame's `Dict_ID` and asserts it matches
`Dictionary#id` before touching the payload. Receivers that maintain
multiple dictionaries can therefore route incoming frames to the
right one purely by parsing the frame header — no out-of-band id
channel needed.

LZ4 dictionaries are always raw bytes (unlike Zstd, there is no
dict-file header format), so there is no header to parse an id out
of. If you need sender and receiver to agree on an id without
shipping it out-of-band, deriving it deterministically from the
dict bytes — which is what `Dictionary.new` does — is the simplest
option.

Dictionary training from a sample corpus is **not supported**: LZ4
has no equivalent of Zstd's `ZDICT_trainFromBuffer`. Dictionaries
are supplied by the caller as raw bytes (typically a hand-picked
prefix or a representative message).

### Ractors

Module functions and `RLZ4::Dictionary` can be used from any Ractor.
**`RLZ4::BlockCodec` cannot cross Ractor boundaries** — allocate one
per Ractor. Example from the test suite:

```ruby
ractors = 4.times.map do |i|
  Ractor.new(i) do |idx|
    pt = "ractor #{idx} payload " * 1000
    1000.times do
      ct = RLZ4.compress_frame(pt)
      raise "mismatch" unless RLZ4.decompress_frame(ct) == pt
    end
    :ok
  end
end
ractors.map(&:value) # => [:ok, :ok, :ok, :ok]
```

## Non-goals

- High-compression mode (LZ4_HC).
- Streaming / chunked compression.
- Preservation of string encoding on decompress (output is always binary).

## License

MIT
