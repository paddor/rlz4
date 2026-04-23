# frozen_string_literal: true

require "digest"

module RLZ4
  # Pure value type for an LZ4 dictionary: raw bytes plus a 4-byte id.
  # Built on `Data.define`, so it's immutable, gets `==` / `#hash` /
  # `#deconstruct` for free, and is shareable across Ractors.
  #
  # The id defaults to `sha256(bytes)[0, 4]` interpreted little-endian
  # — the same derivation LZ4 frame FLG.DictID uses. Callers can pass
  # their own id (e.g. a value coordinated out of band) via `id:`.
  #
  # The id is load-bearing in the frame format (FrameCodec writes it
  # into the FrameDescriptor); BlockCodec accepts a Dictionary for
  # API symmetry but doesn't consult the id.
  Dictionary = Data.define(:bytes, :id) do
    def initialize(bytes:, id: Digest::SHA256.digest(bytes).byteslice(0, 4).unpack1("V"))
      super(bytes: bytes.b.freeze, id: id)
    end


    def size
      bytes.bytesize
    end
  end
end
