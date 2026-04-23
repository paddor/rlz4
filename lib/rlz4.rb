# frozen_string_literal: true

require_relative "rlz4/rlz4"        # Rust extension (native classes + compress_bound)
require_relative "rlz4/version"
require_relative "rlz4/dictionary"
require_relative "rlz4/block_codec"
require_relative "rlz4/frame_codec"
