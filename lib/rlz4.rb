# frozen_string_literal: true

require "digest"

require_relative "rlz4/rlz4"
require_relative "rlz4/version"

module RLZ4
  # --- Frame API compatibility aliases ---
  #
  # 0.3 renamed the frame-format module functions to `compress_frame` /
  # `decompress_frame` to make room for block-format primitives on the
  # module surface. The 0.1/0.2 names are kept for one release cycle.
  module_function

  def compress(bytes)
    compress_frame(bytes)
  end

  def decompress(bytes)
    decompress_frame(bytes)
  end

  class BlockCodec
    # Block-format LZ4 compression with a reusable scratch hash table.
    #
    # Dict is passed once at construction time and baked into the codec:
    # the dict is hashed into a pristine table exactly once in `.new`, and
    # every subsequent `#compress` call restores that pristine state via
    # a 16 KiB memcpy before running the block compressor. This amortises
    # dict initialisation across the codec's lifetime rather than paying
    # ~3–5 µs per call to re-hash the dict.
    #
    # `#compress` mutates the scratch table; `#decompress` does not. Both
    # live on the same class so callers hold one object per worker.
    #
    # A BlockCodec must not cross Ractor boundaries. Per-Ractor codecs are
    # the natural unit.
    def self.new(dict: nil)
      _native_new(dict)
    end

    def decompress(bytes, decompressed_size:)
      _decompress(bytes, decompressed_size)
    end
  end

  class Dictionary
    # Public constructor. Derives the LZ4 frame `Dict_ID` from the dictionary
    # bytes (sha256 truncated to the first 4 bytes, little-endian) and forwards
    # to the Rust extension. The id is what gets written into every emitted
    # frame's FrameDescriptor and what `#decompress` asserts the incoming
    # frame declares before decoding.
    def self.new(bytes)
      id = Digest::SHA256.digest(bytes).byteslice(0, 4).unpack1("V")
      _native_new(bytes, id)
    end
  end
end
