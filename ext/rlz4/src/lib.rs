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

// ---------- Dictionary: dict-bound LZ4 frame compression ----------
//
// Backed by lz4_flex's `FrameEncoder::with_dictionary` /
// `FrameDecoder::with_dictionary` (added in our fork). Output is a real
// LZ4 frame with the FLG.DictID bit set and `Dict_ID` written into the
// FrameDescriptor — interoperable with the reference `lz4` CLI given the
// same dictionary file.
//
// `Dict_ID` is supplied by the caller (the Ruby wrapper in `lib/rlz4.rb`
// derives it from `sha256(dict_bytes)[0..4]` interpreted little-endian).
// Doing the digest in Ruby keeps a hash crate out of the Rust extension's
// dependency tree.
#[magnus::wrap(class = "RLZ4::Dictionary", free_immediately, size)]
struct Dictionary {
    bytes: Vec<u8>,
    id: u32,
}

// Safety: Dictionary is read-only after construction (just a byte buffer
// plus a derived id). No interior mutability, no thread-local refs.
unsafe impl Send for Dictionary {}
unsafe impl Sync for Dictionary {}

fn dict_initialize(_ruby: &Ruby, rb_dict: RString, id: u32) -> Result<Dictionary, Error> {
    // SAFETY: copy bytes into an owned Vec before any Ruby allocation.
    let bytes: Vec<u8> = unsafe { rb_dict.as_slice().to_vec() };
    rb_dict.freeze();
    Ok(Dictionary { bytes, id })
}

fn dict_compress(ruby: &Ruby, rb_self: &Dictionary, rb_input: RString) -> Result<RString, Error> {
    let input: Vec<u8> = unsafe { rb_input.as_slice().to_vec() };
    let upper = lz4_flex::block::get_maximum_output_size(input.len()) + 64;
    let mut encoder = lz4_flex::frame::FrameEncoder::with_dictionary(
        Vec::with_capacity(upper),
        &rb_self.bytes,
        rb_self.id,
    );
    encoder.write_all(&input).map_err(|e| {
        Error::new(
            ruby.exception_runtime_error(),
            format!("lz4 dict frame encode write failed: {e}"),
        )
    })?;
    let compressed = encoder.finish().map_err(|e| {
        Error::new(
            ruby.exception_runtime_error(),
            format!("lz4 dict frame encode finish failed: {e}"),
        )
    })?;
    Ok(ruby.str_from_slice(&compressed))
}

fn dict_decompress(
    ruby: &Ruby,
    rb_self: &Dictionary,
    rb_input: RString,
) -> Result<RString, Error> {
    let compressed: Vec<u8> = unsafe { rb_input.as_slice().to_vec() };
    if compressed.len() < LZ4_FRAME_MAGIC.len() || compressed[..4] != LZ4_FRAME_MAGIC {
        return Err(Error::new(
            decompress_error(ruby),
            "lz4 dict frame decode failed: bad magic (input is not an LZ4 frame)",
        ));
    }

    let mut decoder = lz4_flex::frame::FrameDecoder::with_dictionary(
        &compressed[..],
        &rb_self.bytes,
        rb_self.id,
    );
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).map_err(|e| {
        Error::new(
            decompress_error(ruby),
            format!("lz4 dict frame decode failed: {e}"),
        )
    })?;
    Ok(ruby.str_from_slice(&out))
}

fn dict_size(rb_self: &Dictionary) -> usize {
    rb_self.bytes.len()
}

fn dict_id(rb_self: &Dictionary) -> u32 {
    rb_self.id
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
    // Bound as `_native_new(bytes, id)`. Ruby's `RLZ4::Dictionary.new(bytes)`
    // computes the id and forwards — see `lib/rlz4.rb`.
    dict_class.define_singleton_method("_native_new", function!(dict_initialize, 2))?;
    dict_class.define_method("compress", method!(dict_compress, 1))?;
    dict_class.define_method("decompress", method!(dict_decompress, 1))?;
    dict_class.define_method("size", method!(dict_size, 0))?;
    dict_class.define_method("id", method!(dict_id, 0))?;

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
    fn frame_dict_round_trip() {
        let dict = b"JSON schema version 1 field ".repeat(4);
        let id: u32 = 0xDEAD_BEEF;
        let msg = b"JSON schema version 1 field name=hello value=world".to_vec();

        let mut enc = lz4_flex::frame::FrameEncoder::with_dictionary(Vec::new(), &dict, id);
        enc.write_all(&msg).unwrap();
        let ct = enc.finish().unwrap();
        assert_eq!(&ct[..4], &[0x04, 0x22, 0x4d, 0x18]);

        let mut dec = lz4_flex::frame::FrameDecoder::with_dictionary(&*ct, &dict, id);
        let mut pt = Vec::new();
        dec.read_to_end(&mut pt).unwrap();
        assert_eq!(pt, msg);
    }

    #[test]
    fn frame_dict_id_mismatch_fails() {
        let dict_a = b"common prefix AAA ".repeat(4);
        let dict_b = b"common prefix BBB ".repeat(4);

        let msg = b"common prefix AAA : the payload";
        let mut enc =
            lz4_flex::frame::FrameEncoder::with_dictionary(Vec::new(), &dict_a, 0xAAAA_AAAA);
        enc.write_all(msg).unwrap();
        let ct = enc.finish().unwrap();

        let mut dec =
            lz4_flex::frame::FrameDecoder::with_dictionary(&*ct, &dict_b, 0xBBBB_BBBB);
        let mut out = Vec::new();
        assert!(dec.read_to_end(&mut out).is_err());
    }
}
