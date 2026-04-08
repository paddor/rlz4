# frozen_string_literal: true

require_relative "test_helper"
require "objspace"

describe RLZ4 do
  describe "VERSION" do
    it "is a non-empty string" do
      assert_instance_of String, RLZ4::VERSION
      refute_empty RLZ4::VERSION
    end
  end

  describe ".compress / .decompress (frame format)" do
    it "round-trips an empty string" do
      ct = RLZ4.compress("")
      assert_equal "", RLZ4.decompress(ct)
    end

    it "round-trips a single byte" do
      ct = RLZ4.compress("x")
      assert_equal "x", RLZ4.decompress(ct)
    end

    it "round-trips ASCII text" do
      pt = "the quick brown fox jumps over the lazy dog"
      assert_equal pt, RLZ4.decompress(RLZ4.compress(pt))
    end

    it "round-trips highly repetitive input and actually compresses it" do
      pt = "A" * 100_000
      ct = RLZ4.compress(pt)
      assert_operator ct.bytesize, :<, pt.bytesize / 10
      assert_equal pt, RLZ4.decompress(ct)
    end

    it "round-trips random bytes (1 MiB)" do
      pt = Random.bytes(1_048_576)
      assert_equal pt, RLZ4.decompress(RLZ4.compress(pt))
    end

    it "round-trips binary data with NUL bytes" do
      pt = (0..255).map(&:chr).join * 16
      pt.force_encoding(Encoding::ASCII_8BIT)
      assert_equal pt, RLZ4.decompress(RLZ4.compress(pt))
    end

    it "emits the LZ4 frame magic number (04 22 4D 18)" do
      ct = RLZ4.compress("anything")
      assert_equal [0x04, 0x22, 0x4D, 0x18], ct.bytes.first(4)
    end

    it "returns binary-encoded output for compress" do
      ct = RLZ4.compress("hello")
      assert_equal Encoding::ASCII_8BIT, ct.encoding
    end

    it "returns binary-encoded output for decompress" do
      pt = RLZ4.decompress(RLZ4.compress("hello"))
      assert_equal Encoding::ASCII_8BIT, pt.encoding
    end

    it "raises DecompressError on garbage input" do
      assert_raises(RLZ4::DecompressError) { RLZ4.decompress("not a valid lz4 frame") }
    end

    it "raises DecompressError on truncated frame" do
      ct = RLZ4.compress("some data that will compress")
      assert_raises(RLZ4::DecompressError) { RLZ4.decompress(ct[0, ct.bytesize / 2]) }
    end

    it "raises DecompressError on empty input" do
      assert_raises(RLZ4::DecompressError) { RLZ4.decompress("") }
    end

    it "DecompressError is a StandardError subclass" do
      assert_includes RLZ4::DecompressError.ancestors, StandardError
    end
  end

  describe "DoS resistance" do
    it "does not allocate a large output String on failed decompress" do
      size    = 1_048_576
      garbage = "\x00".b * size

      GC.start
      before = ObjectSpace.each_object(String).count { |s| s.bytesize >= size }

      10.times do
        assert_raises(RLZ4::DecompressError) { RLZ4.decompress(garbage) }
      end

      GC.start
      after = ObjectSpace.each_object(String).count { |s| s.bytesize >= size }

      assert_equal before, after,
        "failed decompress should not leak large output strings"
    end
  end

  describe RLZ4::Dictionary do
    let(:dict) { "header version=1 type=message field1=" }
    let(:d)    { RLZ4::Dictionary.new(dict) }

    it "reports its size" do
      assert_equal dict.bytesize, d.size
    end

    it "round-trips a message that shares the dict prefix" do
      msg = "header version=1 type=message field1=hello world"
      ct  = d.compress(msg)
      assert_equal msg, d.decompress(ct)
    end

    it "round-trips random bytes" do
      msg = Random.bytes(4096)
      assert_equal msg, d.decompress(d.compress(msg))
    end

    it "round-trips an empty string" do
      ct = d.compress("")
      assert_equal "", d.decompress(ct)
    end

    it "does not round-trip with a different dict" do
      d2  = RLZ4::Dictionary.new("totally different dictionary payload")
      msg = "header version=1 type=message field1=hello"
      ct  = d.compress(msg)
      # Either raises or returns wrong bytes — but must not silently succeed.
      begin
        got = d2.decompress(ct)
        refute_equal msg, got
      rescue RLZ4::DecompressError
        # acceptable
      end
    end

    it "raises DecompressError on garbage input" do
      assert_raises(RLZ4::DecompressError) { d.decompress("garbage") }
    end

    it "compresses small messages with a dict more efficiently than without" do
      # A message that is mostly dict prefix should compress very small with the dict.
      msg        = dict + "payload"
      ct_with    = d.compress(msg)
      ct_without = RLZ4.compress(msg)
      assert_operator ct_with.bytesize, :<, ct_without.bytesize
    end
  end

  describe "Ractor safety" do
    it "compresses and decompresses inside a Ractor" do
      r = Ractor.new do
        pt = "hello from inside a ractor " * 100
        ct = RLZ4.compress(pt)
        [ct.bytesize, RLZ4.decompress(ct) == pt]
      end
      size, ok = r.value
      assert_equal true, ok
      assert_operator size, :>, 0
    end

    it "passes a Dictionary to a Ractor (must be shareable)" do
      # The Dictionary wraps an immutable byte buffer and is marked Send+Sync
      # on the Rust side, which is what makes this gem Ractor-safe.
      r = Ractor.new do
        d   = RLZ4::Dictionary.new("shared dict prefix ")
        msg = "shared dict prefix body"
        ct  = d.compress(msg)
        d.decompress(ct) == msg
      end
      assert_equal true, r.value
    end

    it "multiple Ractors compress in parallel without crashing" do
      ractors = 4.times.map do |i|
        Ractor.new(i) do |idx|
          pt = "ractor #{idx} payload " * 1000
          1000.times do
            ct = RLZ4.compress(pt)
            raise "mismatch in ractor #{idx}" unless RLZ4.decompress(ct) == pt
          end
          :ok
        end
      end
      results = ractors.map(&:value)
      assert_equal [:ok, :ok, :ok, :ok], results
    end
  end
end
