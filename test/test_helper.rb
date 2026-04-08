# frozen_string_literal: true

# Silence "Ractor is experimental" warnings that fire on every Ractor.new.
Warning[:experimental] = false

require "minitest/autorun"
require "rlz4"
