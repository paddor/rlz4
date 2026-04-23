# frozen_string_literal: true

require_relative "dictionary"

module RLZ4
  class BlockCodec
    # Block-format LZ4 compression with a reusable scratch hash table.
    #
    # Dict is passed once at construction time and baked into the codec:
    # the dict is hashed into a pristine table exactly once in `.new`, and
    # every subsequent `#compress` call restores that pristine state via
    # a 16 KiB memcpy before running the block compressor. This amortises
    # dict initialisation across the codec's lifetime rather than paying
    # ~3-5 µs per call to re-hash the dict.
    #
    # `#compress` mutates the scratch table; `#decompress` does not. Both
    # live on the same class so callers hold one object per worker.
    #
    # A BlockCodec must not cross Ractor boundaries. Per-Ractor codecs are
    # the natural unit.
    #
    # @param dict [Dictionary, String, nil] dictionary bytes or a
    #   Dictionary value wrapping them. The id on a Dictionary is
    #   ignored (block format has no Dict_ID field); we only consult
    #   the bytes.
    def self.new(dict: nil)
      _native_new(Dictionary === dict ? dict.bytes : dict)
    end


    def decompress(bytes, decompressed_size:)
      _decompress(bytes, decompressed_size)
    end
  end
end
