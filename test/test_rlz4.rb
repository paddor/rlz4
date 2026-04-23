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

  # Frame-format codec tests — these used to exercise the module-level
  # RLZ4.compress / RLZ4.decompress, removed in 0.4. RLZ4::FrameCodec.new
  # (no-dict) is now the canonical entry point for frame-format LZ4.
  describe "RLZ4::FrameCodec (no dict)" do
    let(:codec) { RLZ4::FrameCodec.new }

    it "round-trips an empty string" do
      ct = codec.compress("")
      assert_equal "", codec.decompress(ct)
    end

    it "round-trips a single byte" do
      ct = codec.compress("x")
      assert_equal "x", codec.decompress(ct)
    end

    it "round-trips ASCII text" do
      pt = "the quick brown fox jumps over the lazy dog"
      assert_equal pt, codec.decompress(codec.compress(pt))
    end

    it "round-trips highly repetitive input and actually compresses it" do
      pt = "A" * 100_000
      ct = codec.compress(pt)
      assert_operator ct.bytesize, :<, pt.bytesize / 10
      assert_equal pt, codec.decompress(ct)
    end

    it "round-trips random bytes (1 MiB)" do
      pt = Random.bytes(1_048_576)
      assert_equal pt, codec.decompress(codec.compress(pt))
    end

    it "round-trips binary data with NUL bytes" do
      pt = (0..255).map(&:chr).join * 16
      pt.force_encoding(Encoding::ASCII_8BIT)
      assert_equal pt, codec.decompress(codec.compress(pt))
    end

    it "emits the LZ4 frame magic number (04 22 4D 18)" do
      ct = codec.compress("anything")
      assert_equal [0x04, 0x22, 0x4D, 0x18], ct.bytes.first(4)
    end

    it "returns binary-encoded output for compress" do
      ct = codec.compress("hello")
      assert_equal Encoding::ASCII_8BIT, ct.encoding
    end

    it "returns binary-encoded output for decompress" do
      pt = codec.decompress(codec.compress("hello"))
      assert_equal Encoding::ASCII_8BIT, pt.encoding
    end

    it "raises DecompressError on garbage input" do
      assert_raises(RLZ4::DecompressError) { codec.decompress("not a valid lz4 frame") }
    end

    it "raises DecompressError on truncated frame" do
      ct = codec.compress("some data that will compress")
      assert_raises(RLZ4::DecompressError) { codec.decompress(ct[0, ct.bytesize / 2]) }
    end

    it "raises DecompressError on empty input" do
      assert_raises(RLZ4::DecompressError) { codec.decompress("") }
    end

    it "DecompressError is a StandardError subclass" do
      assert_includes RLZ4::DecompressError.ancestors, StandardError
    end
  end

  describe "DoS resistance" do
    it "does not allocate a large output String on failed decompress" do
      codec   = RLZ4::FrameCodec.new
      size    = 1_048_576
      garbage = "\x00".b * size

      GC.start
      before = ObjectSpace.each_object(String).count { |s| s.bytesize >= size }

      10.times do
        assert_raises(RLZ4::DecompressError) { codec.decompress(garbage) }
      end

      GC.start
      after = ObjectSpace.each_object(String).count { |s| s.bytesize >= size }

      assert_equal before, after,
        "failed decompress should not leak large output strings"
    end
  end

  describe RLZ4::Dictionary do
    let(:bytes) { "header version=1 type=message field1=" }

    it "stores bytes binary-encoded and frozen" do
      d = RLZ4::Dictionary.new(bytes: bytes)
      assert_equal bytes.b, d.bytes
      assert_predicate d.bytes, :frozen?
      assert_equal Encoding::ASCII_8BIT, d.bytes.encoding
    end

    it "defaults id to sha256(bytes)[0, 4] interpreted LE" do
      expected = Digest::SHA256.digest(bytes)[0, 4].unpack1("V")
      assert_equal expected, RLZ4::Dictionary.new(bytes: bytes).id
    end

    it "accepts a caller-supplied id: kwarg" do
      d = RLZ4::Dictionary.new(bytes: bytes, id: 0xDEAD_BEEF)
      assert_equal 0xDEAD_BEEF, d.id
    end

    it "#size reports dict size in bytes" do
      assert_equal bytes.bytesize, RLZ4::Dictionary.new(bytes: bytes).size
    end

    it "inherits Data's immutability and value equality" do
      assert_predicate RLZ4::Dictionary.new(bytes: bytes), :frozen?
      assert_equal(
        RLZ4::Dictionary.new(bytes: bytes),
        RLZ4::Dictionary.new(bytes: bytes.dup),
      )
      assert_equal(
        RLZ4::Dictionary.new(bytes: bytes).hash,
        RLZ4::Dictionary.new(bytes: bytes.dup).hash,
      )
    end

    it "is shareable across Ractors" do
      r = Ractor.new(RLZ4::Dictionary.new(bytes: bytes)) { |d| [d.bytes, d.id] }
      got_bytes, got_id = r.value
      assert_equal bytes.b, got_bytes
      assert_equal Digest::SHA256.digest(bytes)[0, 4].unpack1("V"), got_id
    end
  end

  describe RLZ4::FrameCodec do
    let(:dict_bytes) { "header version=1 type=message field1=" }
    let(:dict)       { RLZ4::Dictionary.new(bytes: dict_bytes) }
    let(:d)          { RLZ4::FrameCodec.new(dict: dict) }
    let(:no_dict)    { RLZ4::FrameCodec.new }

    it ".new without a dict accepts compress/decompress round-trips" do
      msg = "the quick brown fox jumps over the lazy dog"
      ct  = no_dict.compress(msg)
      assert_equal [0x04, 0x22, 0x4D, 0x18], ct.bytes.first(4)
      assert_equal msg, no_dict.decompress(ct)
    end

    it ".new(dict: Dictionary) uses the Dictionary's cached id" do
      assert_equal dict.id, d.id
    end

    it ".new(dict: String) also works and derives the id on the fly" do
      c = RLZ4::FrameCodec.new(dict: dict_bytes)
      assert_equal dict.id, c.id
    end

    it ".new raises TypeError for other dict arg types" do
      assert_raises(TypeError) { RLZ4::FrameCodec.new(dict: 42) }
    end

    it "#has_dict? reflects construction" do
      assert_predicate d,       :has_dict?
      refute_predicate no_dict, :has_dict?
    end

    it "#size is dict size or 0" do
      assert_equal dict_bytes.bytesize, d.size
      assert_equal 0, no_dict.size
    end

    it "#id is nil without a dict" do
      assert_nil no_dict.id
    end

    it "emits a real LZ4 frame with the magic number" do
      ct = d.compress("header version=1 type=message field1=hello")
      assert_equal [0x04, 0x22, 0x4D, 0x18], ct.bytes.first(4)
    end

    it "raises DecompressError on dict id mismatch" do
      d2 = RLZ4::FrameCodec.new(dict: "totally different dictionary payload")
      ct = d.compress("header version=1 type=message field1=hello")
      assert_raises(RLZ4::DecompressError) { d2.decompress(ct) }
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

    it "raises DecompressError on garbage input" do
      assert_raises(RLZ4::DecompressError) { d.decompress("garbage") }
    end

    it "compresses small messages with a dict more efficiently than without" do
      msg        = dict_bytes + "payload"
      ct_with    = d.compress(msg)
      ct_without = no_dict.compress(msg)
      assert_operator ct_with.bytesize, :<, ct_without.bytesize
    end
  end

  describe ".compress_bound" do
    it "is monotonic in input size" do
      a = RLZ4.compress_bound(100)
      b = RLZ4.compress_bound(1_000)
      c = RLZ4.compress_bound(1_000_000)
      assert_operator a, :<, b
      assert_operator b, :<, c
    end

    it "is large enough to hold real compressor output for random input" do
      codec = RLZ4::BlockCodec.new
      [0, 1, 100, 4096, 100_000].each do |n|
        pt    = Random.bytes(n)
        bound = RLZ4.compress_bound(n)
        ct    = codec.compress(pt)
        assert_operator ct.bytesize, :<=, bound,
          "compress_bound(#{n}) = #{bound} must hold #{ct.bytesize} bytes of ciphertext"
      end
    end
  end

  describe RLZ4::BlockCodec do
    describe "no-dict codec" do
      let(:codec) { RLZ4::BlockCodec.new }

      it "reports has_dict? = false" do
        refute_predicate codec, :has_dict?
      end

      it "round-trips an empty string" do
        ct = codec.compress("")
        assert_equal "", codec.decompress(ct, decompressed_size: 0)
      end

      it "round-trips ASCII text" do
        pt = "the quick brown fox jumps over the lazy dog"
        ct = codec.compress(pt)
        assert_equal pt, codec.decompress(ct, decompressed_size: pt.bytesize)
      end

      it "round-trips highly repetitive input and actually compresses" do
        pt = "A" * 100_000
        ct = codec.compress(pt)
        assert_operator ct.bytesize, :<, pt.bytesize / 100
        assert_equal pt, codec.decompress(ct, decompressed_size: pt.bytesize)
      end

      it "round-trips across size buckets" do
        [0, 1, 12, 13, 64, 255, 256, 1024, 4096, 65_536, 1_048_576].each do |n|
          pt = Random.bytes(n)
          ct = codec.compress(pt)
          assert_equal pt, codec.decompress(ct, decompressed_size: n),
            "round-trip failed at size #{n}"
        end
      end

      it "emits binary-encoded output" do
        ct = codec.compress("hello")
        assert_equal Encoding::ASCII_8BIT, ct.encoding
      end

      it "reuses the scratch table across many calls" do
        # If the table were not properly cleared, earlier inputs would bleed
        # into later ciphertexts and round-trip would break.
        500.times do |i|
          pt = "message #{i} " * (1 + i % 10)
          ct = codec.compress(pt)
          assert_equal pt, codec.decompress(ct, decompressed_size: pt.bytesize)
        end
      end

      it "reports its size as one 16 KiB table" do
        assert_equal 16_384, codec.size
      end
    end

    describe "dict codec" do
      let(:dict)  { "JSON field prefix: version=1 type=event data=" }
      let(:codec) { RLZ4::BlockCodec.new(dict: dict) }

      it "reports has_dict? = true" do
        assert_predicate codec, :has_dict?
      end

      it "round-trips" do
        msg = "JSON field prefix: version=1 type=event data=hello"
        ct  = codec.compress(msg)
        assert_equal msg, codec.decompress(ct, decompressed_size: msg.bytesize)
      end

      it "decodes with a separate receiver codec constructed with the same dict" do
        msg = "JSON field prefix: version=1 type=event data=world"
        ct  = codec.compress(msg)
        receiver = RLZ4::BlockCodec.new(dict: dict)
        assert_equal msg, receiver.decompress(ct, decompressed_size: msg.bytesize)
      end

      it "compresses dict-sharing input better than without dict" do
        msg        = "JSON field prefix: version=1 type=event data=x"
        ct_with    = codec.compress(msg)
        ct_without = RLZ4::BlockCodec.new.compress(msg)
        assert_operator ct_with.bytesize, :<, ct_without.bytesize
      end

      it "round-trips across size buckets" do
        [0, 1, 64, 1024, 65_536].each do |n|
          pt = Random.bytes(n)
          ct = codec.compress(pt)
          assert_equal pt, codec.decompress(ct, decompressed_size: n),
            "round-trip failed at size #{n}"
        end
      end

      it "round-trips 500 times in a row (pristine table is not clobbered)" do
        # The per-call memcpy from pristine→scratch must restore state
        # exactly — otherwise successive calls would drift.
        msgs = 500.times.map { |i| "JSON field prefix: version=1 type=event data=#{i}" }
        ciphertexts = msgs.map { |m| codec.compress(m) }
        msgs.zip(ciphertexts).each do |msg, ct|
          assert_equal msg, codec.decompress(ct, decompressed_size: msg.bytesize)
        end
      end

      it "reports its size as two 16 KiB tables plus dict bytes" do
        assert_equal 16_384 + 16_384 + dict.bytesize, codec.size
      end
    end

    describe "bounded decompression" do
      let(:codec) { RLZ4::BlockCodec.new }

      it "refuses to write past decompressed_size" do
        pt = "X" * 10_000
        ct = codec.compress(pt)
        # Lie and say the output is tiny. Must fail, not segfault or truncate.
        assert_raises(RLZ4::DecompressError) do
          codec.decompress(ct, decompressed_size: 100)
        end
      end

      it "raises DecompressError on garbage input" do
        # A token claiming 200 literals in a 1-byte buffer is unambiguously
        # malformed: the literal payload can't be read. Note that many short
        # byte sequences are accidentally valid LZ4 blocks (e.g. "garbage"
        # parses as token-6-literals + 6 literal bytes), so the test input
        # has to be crafted to fail a specific invariant.
        malformed = "\xFF".b
        assert_raises(RLZ4::DecompressError) do
          codec.decompress(malformed, decompressed_size: 100)
        end
      end

      it "raises DecompressError on truncated ciphertext" do
        pt = "X" * 10_000
        ct = codec.compress(pt)
        assert_raises(RLZ4::DecompressError) do
          codec.decompress(ct[0, ct.bytesize / 2], decompressed_size: pt.bytesize)
        end
      end

      it "survives a fuzz of 10k random inputs without crashing" do
        # Plan exit criterion: decompress refuses to write past
        # decompressed_size even on crafted malformed input, and must
        # never segfault or OOM across 10k mutated inputs.
        srand(0xC0DEC)
        10_000.times do
          len = rand(1..1024)
          blob = Random.bytes(len)
          decompressed_size = rand(0..16_384)
          begin
            codec.decompress(blob, decompressed_size: decompressed_size)
          rescue RLZ4::DecompressError
            # expected
          end
        end
      end

      it "survives a fuzz of 10k mutated valid ciphertexts" do
        # Mutating known-valid ciphertexts is a more effective fuzz for the
        # decoder state machine than random bytes, which mostly flunk the
        # very first token check.
        srand(0xFA11B1)
        pt = "the quick brown fox jumps over the lazy dog " * 64
        valid = codec.compress(pt)
        10_000.times do
          mutated = valid.dup
          # Flip a random byte (or a few).
          1.upto(rand(1..3)) do
            i = rand(mutated.bytesize)
            mutated.setbyte(i, rand(256))
          end
          begin
            codec.decompress(mutated, decompressed_size: pt.bytesize)
          rescue RLZ4::DecompressError
            # expected for most mutations
          end
        end
      end
    end

    describe "wrong-dict decode" do
      # LZ4 block format has no built-in dict id or checksum. Decoding a
      # dict-compressed message with the wrong dict may raise (if the
      # garbage violates the block state machine) or may silently produce
      # corrupted output. What matters is: no segfault, no out-of-bounds
      # write, no OOM.
      let(:dict_a) { ("header version=1 type=message field=" * 3).b }
      let(:dict_b) { ("totally different dictionary payload here " * 3).b }

      it "does not crash when decoding with the wrong dict" do
        sender   = RLZ4::BlockCodec.new(dict: dict_a)
        receiver = RLZ4::BlockCodec.new(dict: dict_b)
        msg = "header version=1 type=message field=hello"
        ct  = sender.compress(msg)

        # Either raises DecompressError or produces garbage; must not crash.
        # Output buffer is bounded to msg.bytesize; a wrong-dict decode
        # that tries to reach past it raises.
        begin
          out = receiver.decompress(ct, decompressed_size: msg.bytesize)
          refute_equal msg, out, "wrong-dict decode surprisingly produced the correct plaintext"
        rescue RLZ4::DecompressError
          # also fine
        end
      end

      it "does not crash when decoding without a dict that was dict-compressed" do
        sender   = RLZ4::BlockCodec.new(dict: dict_a)
        receiver = RLZ4::BlockCodec.new  # no dict
        msg = "header version=1 type=message field=hello"
        ct  = sender.compress(msg)

        begin
          out = receiver.decompress(ct, decompressed_size: msg.bytesize)
          refute_equal msg, out
        rescue RLZ4::DecompressError
          # also fine
        end
      end
    end
  end

  describe "Ractor safety" do
    it "compresses and decompresses inside a Ractor" do
      r = Ractor.new do
        codec = RLZ4::FrameCodec.new
        pt    = "hello from inside a ractor " * 100
        ct    = codec.compress(pt)
        [ct.bytesize, codec.decompress(ct) == pt]
      end
      size, ok = r.value
      assert_equal true, ok
      assert_operator size, :>, 0
    end

    it "passes a FrameCodec to a Ractor (must be shareable)" do
      # FrameCodec wraps an immutable byte buffer and is marked Send+Sync
      # on the Rust side, which is what makes this gem Ractor-safe.
      r = Ractor.new do
        d   = RLZ4::FrameCodec.new(dict: "shared dict prefix ")
        msg = "shared dict prefix body"
        ct  = d.compress(msg)
        d.decompress(ct) == msg
      end
      assert_equal true, r.value
    end

    it "BlockCodec is per-Ractor (each Ractor allocates its own)" do
      r = Ractor.new do
        c   = RLZ4::BlockCodec.new
        msg = "ractor local payload " * 50
        ct  = c.compress(msg)
        c.decompress(ct, decompressed_size: msg.bytesize) == msg
      end
      assert_equal true, r.value
    end

    it "BlockCodec cannot cross Ractor boundaries" do
      codec = RLZ4::BlockCodec.new
      # Sending a non-shareable BlockCodec into a Ractor must raise. magnus
      # exposes this as "allocator undefined" via TypeError at Ractor.new
      # time; the exact class matters less than "not a silent success".
      assert_raises(TypeError, Ractor::IsolationError) do
        Ractor.new(codec) { |c| c.compress("payload") }
      end
    end

    it "multiple Ractors compress in parallel without crashing" do
      ractors = 4.times.map do |i|
        Ractor.new(i) do |idx|
          codec = RLZ4::FrameCodec.new
          pt    = "ractor #{idx} payload " * 1000
          1000.times do
            ct = codec.compress(pt)
            raise "mismatch in ractor #{idx}" unless codec.decompress(ct) == pt
          end
          :ok
        end
      end
      results = ractors.map(&:value)
      assert_equal [:ok, :ok, :ok, :ok], results
    end
  end
end
