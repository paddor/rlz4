# frozen_string_literal: true

$LOAD_PATH.unshift File.expand_path("../lib", __dir__)
require "rlz4"

ITERS  = 2_000
WARMUP = 200

SIZES  = { "1 KB" => 1_024, "16 KB" => 16_384, "256 KB" => 262_144 }
DICT   = ("JSON field prefix: version=1 type=event data=" * 40).freeze

def bench(label, iters, &blk)
  iters.times(&blk)                              # warmup
  t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC)
  iters.times(&blk)
  elapsed = Process.clock_gettime(Process::CLOCK_MONOTONIC) - t0
  [label, elapsed / iters]
end

def mbs(bytes, ns_per_op)
  (bytes / ns_per_op / 1_048_576.0).round(1)
end

rows = []

SIZES.each do |size_label, n|
  plain   = Random.bytes(n)
  dict    = RLZ4::Dictionary.new(DICT)

  # Block: no dict
  bc_nd   = RLZ4::BlockCodec.new
  ct_nd   = bc_nd.compress(plain)
  rows << bench("block compress #{size_label}", ITERS) { bc_nd.compress(plain) }
  rows << bench("block decomp  #{size_label}", ITERS) { bc_nd.decompress(ct_nd, decompressed_size: n) }

  # Block: with dict
  bc_d    = RLZ4::BlockCodec.new(dict: DICT)
  ct_d    = bc_d.compress(plain)
  rows << bench("block compress #{size_label} +dict", ITERS) { bc_d.compress(plain) }
  rows << bench("block decomp  #{size_label} +dict", ITERS) { bc_d.decompress(ct_d, decompressed_size: n) }

  # Frame: no dict
  fc_nd   = RLZ4::FrameCodec.new
  ft_nd   = fc_nd.compress(plain)
  rows << bench("frame compress #{size_label}", ITERS) { fc_nd.compress(plain) }
  rows << bench("frame decomp  #{size_label}", ITERS) { fc_nd.decompress(ft_nd) }

  # Frame: with dict
  fc_d    = RLZ4::FrameCodec.new(dict: dict)
  ft_d    = fc_d.compress(plain)
  rows << bench("frame compress #{size_label} +dict", ITERS) { fc_d.compress(plain) }
  rows << bench("frame decomp  #{size_label} +dict", ITERS) { fc_d.decompress(ft_d) }
end

w = rows.map { |r| r[0].length }.max
rows.each do |label, sec_per_op|
  us = (sec_per_op * 1_000_000).round(2)
  printf "  %-#{w}s  %8.2f µs/op\n", label, us
end
