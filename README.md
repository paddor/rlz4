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

compressed   = RLZ4.compress("hello world" * 100)
decompressed = RLZ4.decompress(compressed)

# Wire format is standard LZ4 frame (magic number 04 22 4D 18),
# interoperable with any other LZ4 frame implementation.
```

Invalid input raises `RLZ4::DecompressError` (a `StandardError` subclass):

```ruby
begin
  RLZ4.decompress("not a valid lz4 frame")
rescue RLZ4::DecompressError => e
  warn e.message
end
```

### Dictionary compression

For workloads where many small messages share a common prefix (e.g. ZMQ
messages with a fixed header), a shared dictionary massively improves the
compression ratio. `RLZ4::Dictionary` uses LZ4 **block** format with the
original size prepended — this is a different wire format from
`RLZ4.compress` and is not interoperable with it.

```ruby
dict = RLZ4::Dictionary.new("schema=v1 type=message field1=")

compressed   = dict.compress("schema=v1 type=message field1=payload")
decompressed = dict.decompress(compressed)

dict.size  # => 30
```

`RLZ4::Dictionary` is immutable after construction and can be shared across
Ractors.

### Ractors

Both the module functions and `RLZ4::Dictionary` can be used from any
Ractor. Example from the test suite:

```ruby
ractors = 4.times.map do |i|
  Ractor.new(i) do |idx|
    pt = "ractor #{idx} payload " * 1000
    1000.times do
      ct = RLZ4.compress(pt)
      raise "mismatch" unless RLZ4.decompress(ct) == pt
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
