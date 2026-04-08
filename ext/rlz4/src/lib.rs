use magnus::{
    exception::ExceptionClass,
    function, method,
    prelude::*,
    r_string::RString,
    value::Opaque,
    Error, Ruby,
};
use std::io::{Read, Write};
use std::sync::OnceLock;

use lz4_flex::frame::{FrameDecoder, FrameEncoder};

const LZ4_FRAME_MAGIC: [u8; 4] = [0x04, 0x22, 0x4d, 0x18];

// Opaque<T> is Send+Sync and is designed for storing Ruby values in statics.
static DECOMPRESS_ERROR: OnceLock<Opaque<ExceptionClass>> = OnceLock::new();

fn decompress_error(ruby: &Ruby) -> ExceptionClass {
    ruby.get_inner(
        *DECOMPRESS_ERROR
            .get()
            .expect("DecompressError not initialized"),
    )
}

// ---------- module functions: frame-format compress/decompress ----------

fn rlz4_compress(ruby: &Ruby, rb_input: RString) -> Result<RString, Error> {
    // SAFETY: copy borrowed bytes into an owned Vec before any Ruby allocation.
    let input: Vec<u8> = unsafe { rb_input.as_slice().to_vec() };

    // Pre-size the output buffer. Frame overhead is ~19 bytes for the header
    // plus up to ~4 bytes per block end-marker — 64 is a comfortable ceiling.
    let upper = lz4_flex::block::get_maximum_output_size(input.len()) + 64;
    let mut encoder = FrameEncoder::new(Vec::with_capacity(upper));
    encoder.write_all(&input).map_err(|e| {
        Error::new(
            ruby.exception_runtime_error(),
            format!("lz4 frame encoder write failed: {e}"),
        )
    })?;
    let compressed = encoder.finish().map_err(|e| {
        Error::new(
            ruby.exception_runtime_error(),
            format!("lz4 frame encoder finish failed: {e}"),
        )
    })?;

    Ok(ruby.str_from_slice(&compressed))
}

fn rlz4_decompress(ruby: &Ruby, rb_input: RString) -> Result<RString, Error> {
    // SAFETY: copy borrowed bytes before any Ruby allocation.
    let compressed: Vec<u8> = unsafe { rb_input.as_slice().to_vec() };

    // Reject anything that isn't a well-formed frame up front. lz4_flex's
    // FrameDecoder permissively returns Ok for zero-length input, which would
    // quietly mask "sender forgot --compress" mistakes in omq-cli.
    if compressed.len() < LZ4_FRAME_MAGIC.len() || compressed[..4] != LZ4_FRAME_MAGIC {
        return Err(Error::new(
            decompress_error(ruby),
            "lz4 frame decode failed: bad magic (input is not an LZ4 frame)",
        ));
    }

    // Decode into a local Vec first. If this fails, we never allocate a
    // Ruby string — important for DoS-resistance against malformed input.
    let mut decoder = FrameDecoder::new(&compressed[..]);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).map_err(|e| {
        Error::new(
            decompress_error(ruby),
            format!("lz4 frame decode failed: {e}"),
        )
    })?;

    Ok(ruby.str_from_slice(&out))
}

// ---------- Dictionary: block-format compression with a shared dictionary ----------
//
// lz4_flex's frame format does not implement dictionary-based compression
// (FrameInfo::dict_id is metadata-only). For the small-ZMQ-message use case
// that motivates this class, block format with a prepended size is a better
// fit anyway: lower per-message overhead and direct dictionary support.
//
// Output is a raw LZ4 block with the original (uncompressed) size prepended
// as a little-endian u32, matching lz4_flex's `*_size_prepended` API.
#[magnus::wrap(class = "RLZ4::Dictionary", free_immediately, size)]
struct Dictionary {
    bytes: Vec<u8>,
}

// Safety: Dictionary is read-only after construction (just a byte buffer).
// No interior mutability, no references to thread-local data.
unsafe impl Send for Dictionary {}
unsafe impl Sync for Dictionary {}

fn dict_initialize(_ruby: &Ruby, rb_dict: RString) -> Result<Dictionary, Error> {
    // SAFETY: copy bytes into an owned Vec before any Ruby allocation.
    let bytes: Vec<u8> = unsafe { rb_dict.as_slice().to_vec() };
    rb_dict.freeze();
    Ok(Dictionary { bytes })
}

fn dict_compress(ruby: &Ruby, rb_self: &Dictionary, rb_input: RString) -> Result<RString, Error> {
    let input: Vec<u8> = unsafe { rb_input.as_slice().to_vec() };
    let compressed = lz4_flex::block::compress_prepend_size_with_dict(&input, &rb_self.bytes);
    Ok(ruby.str_from_slice(&compressed))
}

fn dict_decompress(
    ruby: &Ruby,
    rb_self: &Dictionary,
    rb_input: RString,
) -> Result<RString, Error> {
    let compressed: Vec<u8> = unsafe { rb_input.as_slice().to_vec() };
    let out = lz4_flex::block::decompress_size_prepended_with_dict(&compressed, &rb_self.bytes)
        .map_err(|e| {
            Error::new(
                decompress_error(ruby),
                format!("lz4 block decode failed: {e}"),
            )
        })?;
    Ok(ruby.str_from_slice(&out))
}

fn dict_size(rb_self: &Dictionary) -> usize {
    rb_self.bytes.len()
}

// ---------- module init ----------

#[magnus::init]
fn init(ruby: &Ruby) -> Result<(), Error> {
    // Mark this extension as Ractor-safe. All our Rust code uses only
    // stack/owned data, holds no globals aside from the Opaque exception
    // class (which is Send+Sync by construction), and the Dictionary type
    // is read-only after init, so it is safe to call from any Ractor.
    unsafe { rb_sys::rb_ext_ractor_safe(true) };

    let module = ruby.define_module("RLZ4")?;

    let decompress_error_class =
        module.define_error("DecompressError", ruby.exception_standard_error())?;
    DECOMPRESS_ERROR
        .set(Opaque::from(decompress_error_class))
        .unwrap_or_else(|_| panic!("init called more than once"));

    module.define_module_function("compress", function!(rlz4_compress, 1))?;
    module.define_module_function("decompress", function!(rlz4_decompress, 1))?;

    let dict_class = module.define_class("Dictionary", ruby.class_object())?;
    dict_class.define_singleton_method("new", function!(dict_initialize, 1))?;
    dict_class.define_method("compress", method!(dict_compress, 1))?;
    dict_class.define_method("decompress", method!(dict_decompress, 1))?;
    dict_class.define_method("size", method!(dict_size, 0))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip() {
        let data = b"the quick brown fox jumps over the lazy dog ".repeat(100);
        let mut enc = FrameEncoder::new(Vec::new());
        enc.write_all(&data).unwrap();
        let ct = enc.finish().unwrap();
        assert!(ct.len() < data.len(), "should compress repetitive input");
        // Frame magic number
        assert_eq!(&ct[..4], &[0x04, 0x22, 0x4d, 0x18]);

        let mut dec = FrameDecoder::new(&ct[..]);
        let mut out = Vec::new();
        dec.read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn frame_empty_round_trip() {
        let mut enc = FrameEncoder::new(Vec::new());
        enc.write_all(b"").unwrap();
        let ct = enc.finish().unwrap();
        let mut dec = FrameDecoder::new(&ct[..]);
        let mut out = Vec::new();
        dec.read_to_end(&mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn frame_garbage_fails() {
        // A buffer that is long enough to look like a frame but has the
        // wrong magic number must fail to decode.
        let garbage = vec![0xFFu8; 32];
        let mut dec = FrameDecoder::new(&garbage[..]);
        let mut out = Vec::new();
        assert!(dec.read_to_end(&mut out).is_err());
    }

    #[test]
    fn block_dict_round_trip() {
        let dict = b"JSON schema version 1 field ";
        let msg = b"JSON schema version 1 field name=hello value=world";
        let ct = lz4_flex::block::compress_prepend_size_with_dict(msg, dict);
        let pt = lz4_flex::block::decompress_size_prepended_with_dict(&ct, dict).unwrap();
        assert_eq!(pt, msg);
    }

    #[test]
    fn block_dict_mismatch_fails_or_returns_wrong_data() {
        // With a wrong dict, decode either errors out or returns wrong bytes.
        // Either way it must not silently round-trip to the original.
        let dict_a = b"common prefix AAA ";
        let dict_b = b"common prefix BBB ";
        let msg = b"common prefix AAA : the payload";
        let ct = lz4_flex::block::compress_prepend_size_with_dict(msg, dict_a);
        match lz4_flex::block::decompress_size_prepended_with_dict(&ct, dict_b) {
            Ok(out) => assert_ne!(out, msg),
            Err(_) => {}
        }
    }
}
