# frozen_string_literal: true

require_relative "dictionary"

module RLZ4
  class FrameCodec
    # Frame-format LZ4 codec, optionally dict-bound. Parallel in shape
    # to BlockCodec, but emits a real LZ4 frame (magic `04 22 4D 18`)
    # with the FLG.DictID bit set and `Dict_ID` written into the
    # FrameDescriptor when a dict is installed. Output is interoperable
    # with the reference `lz4` CLI given the same dictionary file.
    #
    # Unlike BlockCodec, FrameCodec holds no thread-local mutable state:
    # it's a read-only dict bytes buffer plus a derived id. Shareable
    # across Ractors.
    #
    # @param dict [Dictionary, String, nil] dictionary bytes or a
    #   Dictionary value. Passing a Dictionary reuses its cached id
    #   (skips the sha256 digest); a raw String derives the id on the
    #   fly.
    def self.new(dict: nil)
      case dict
      when nil
        _native_new(nil, 0)
      when Dictionary
        _native_new(dict.bytes, dict.id)
      when String
        _native_new(dict, Dictionary.new(bytes: dict).id)
      else
        raise TypeError, "expected RLZ4::Dictionary, String, or nil; got #{dict.class}"
      end
    end
  end
end
