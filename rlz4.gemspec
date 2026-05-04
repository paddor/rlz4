# frozen_string_literal: true

require_relative "lib/rlz4/version"

Gem::Specification.new do |spec|
  spec.name          = "rlz4"
  spec.version       = RLZ4::VERSION
  spec.authors       = ["Patrik Wenger"]
  spec.email         = ["paddor@protonmail.ch"]

  spec.summary       = "Ractor-safe LZ4 bindings for Ruby (Rust extension via lz4-sys/liblz4)"
  spec.description   = <<~DESC
    Ruby bindings (via Rust/magnus) for liblz4, using lz4-sys FFI.
    Provides block-format and frame-format LZ4 compress/decompress with
    optional dictionary support. Designed to be safe to call from multiple
    Ractors, unlike existing Ruby LZ4 gems.
  DESC
  spec.homepage      = "https://github.com/paddor/rlz4"
  spec.license       = "MIT"

  spec.required_ruby_version = ">= 4.0.0"

  spec.metadata["homepage_uri"]    = spec.homepage
  spec.metadata["source_code_uri"] = spec.homepage

  spec.files = Dir[
    "lib/**/*.rb",
    "ext/**/*.{rs,rb}",
    "ext/**/Cargo.toml",
    "Cargo.toml",
    "Cargo.lock",
    "LICENSE",
    "README.md"
  ]

  spec.require_paths = ["lib"]
  spec.extensions    = ["ext/rlz4/extconf.rb"]

  spec.add_dependency "rb_sys", "~> 0.9"

  spec.add_development_dependency "rake",          "~> 13.0"
  spec.add_development_dependency "rake-compiler", "~> 1.2"
  spec.add_development_dependency "minitest",      "~> 5.0"
end
