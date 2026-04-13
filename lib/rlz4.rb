# frozen_string_literal: true

require "digest"

require_relative "rlz4/rlz4"
require_relative "rlz4/version"

module RLZ4
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
