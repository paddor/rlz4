use magnus::{
    exception::ExceptionClass,
    function, method,
    prelude::*,
    r_string::RString,
    value::Opaque,
    Error, Ruby,
};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::sync::OnceLock;

use lz4_flex::block::{
    compress_into_with_loaded_table_and_dict, compress_into_with_table, decompress_into,
    decompress_into_with_dict, get_maximum_output_size, CompressTable,
};
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

// ---------- module function: compress_bound ----------

fn rlz4_compress_bound(_ruby: &Ruby, size: usize) -> Result<usize, Error> {
    Ok(get_maximum_output_size(size))
}

// ---------- BlockCodec: reusable LZ4 block-format scratch ----------
//
// Wraps lz4_flex's CompressTable. A codec constructed without a dict
// carries one scratch table (cleared per compress call). A codec
// constructed with a dict also carries a pristine table populated with
// dict positions once via `load_dict`; before each compress call the
// scratch table is overwritten from the pristine table with a single
// memcpy. This avoids the ~3–5 µs per-call `init_dict` cost that a naive
// "hash the dict on every call" approach would incur.
//
// Decompression ignores the tables (LZ4 block decoding needs no scratch);
// it lives on the same class so callers hold one object per worker
// instead of two.
//
// Thread-local by construction (RefCell, no Send+Sync). A BlockCodec must
// not cross Ractor boundaries — send a new one instead.

struct DictState {
    bytes: Vec<u8>,
    pristine: CompressTable,
}

#[magnus::wrap(class = "RLZ4::BlockCodec", free_immediately, size)]
struct BlockCodec {
    scratch: RefCell<CompressTable>,
    dict: Option<DictState>,
}

fn block_codec_new(_ruby: &Ruby, rb_dict: Option<RString>) -> Result<BlockCodec, Error> {
    // Large table: 4096 × u32 entries = 16 KiB. Covers any input size without
    // the transparent upgrade path taken by Small. Predictable footprint is
    // more important than the ~8 KiB saving for short-message workloads.
    let scratch = RefCell::new(CompressTable::large());

    let dict = match rb_dict {
        None => None,
        Some(rb_dict) => {
            // SAFETY: copy dict bytes into an owned Vec before any Ruby
            // allocation. The dict lives for the codec's lifetime.
            let bytes: Vec<u8> = unsafe { rb_dict.as_slice().to_vec() };
            let mut pristine = CompressTable::large();
            pristine.load_dict(&bytes);
            Some(DictState { bytes, pristine })
        }
    };

    Ok(BlockCodec { scratch, dict })
}

fn block_codec_size(rb_self: &BlockCodec) -> usize {
    // 4096 entries × 4 bytes = 16 KiB for the scratch table, plus another
    // 16 KiB for the pristine table if a dict is installed, plus the dict
    // bytes themselves.
    let base = 16 * 1024;
    match &rb_self.dict {
        None => base,
        Some(d) => base + 16 * 1024 + d.bytes.len(),
    }
}

fn block_codec_has_dict(rb_self: &BlockCodec) -> bool {
    rb_self.dict.is_some()
}

fn block_codec_compress(
    ruby: &Ruby,
    rb_self: &BlockCodec,
    rb_input: RString,
) -> Result<RString, Error> {
    // SAFETY: borrow the RString's bytes directly, skipping the
    // customary copy-to-Vec. Valid because:
    //   (a) `rb_input` is a stack-pinned argument — the Ruby GC won't
    //       collect or move it while this function runs.
    //   (b) lz4_flex's block compress does no callbacks into Ruby and
    //       no allocations that could trigger Ruby GC — it only
    //       allocates Rust Vecs via the global allocator.
    //   (c) The Ruby string allocation (`str_from_slice`) happens
    //       strictly after the input slice is no longer in use.
    // Saves one input-sized memcpy per call (~1 KiB / ~10ns-100ns).
    let input: &[u8] = unsafe { rb_input.as_slice() };

    let upper = get_maximum_output_size(input.len());
    let mut out = vec![0u8; upper];

    let mut scratch = rb_self.scratch.borrow_mut();
    let compressed_len = match &rb_self.dict {
        None => compress_into_with_table(input, &mut out, &mut scratch),
        Some(d) => {
            // Restore the scratch table to the pristine dict-loaded state.
            // This is one 16 KiB memcpy — ~50× cheaper than re-hashing the
            // dict into a cleared table.
            scratch.copy_from(&d.pristine);
            compress_into_with_loaded_table_and_dict(input, &mut out, &mut scratch, &d.bytes)
        }
    }
    .map_err(|e| {
        Error::new(
            ruby.exception_runtime_error(),
            format!("lz4 block compress failed: {e}"),
        )
    })?;

    out.truncate(compressed_len);
    Ok(ruby.str_from_slice(&out))
}

fn block_codec_decompress(
    ruby: &Ruby,
    rb_self: &BlockCodec,
    rb_input: RString,
    decompressed_size: usize,
) -> Result<RString, Error> {
    // SAFETY: see block_codec_compress. Same reasoning: decoder is pure
    // Rust, no Ruby callbacks, no Ruby allocations until `str_from_slice`.
    let compressed: &[u8] = unsafe { rb_input.as_slice() };

    // Pre-size the output buffer to the caller-supplied decompressed_size.
    // `decompress_into` refuses to write past this boundary (OutputTooSmall),
    // which bounds the DoS window: a malicious sender who lies about
    // decompressed_size gets capped at whatever the caller allowed.
    let mut out = vec![0u8; decompressed_size];

    let actual_len = match &rb_self.dict {
        None => decompress_into(compressed, &mut out),
        Some(d) => decompress_into_with_dict(compressed, &mut out, &d.bytes),
    }
    .map_err(|e| {
        Error::new(
            decompress_error(ruby),
            format!("lz4 block decode failed: {e}"),
        )
    })?;

    out.truncate(actual_len);
    Ok(ruby.str_from_slice(&out))
}

// ---------- FrameCodec: LZ4 frame-format codec, optionally dict-bound ----------
//
// Parallel to BlockCodec (block format). Output is a real LZ4 frame —
// magic `04 22 4D 18`. When constructed with a dict, sets FLG.DictID
// and writes `Dict_ID` into the FrameDescriptor; the dict is stored
// once and consulted on every compress/decompress call.
//
// Backed by lz4_flex's `FrameEncoder` / `FrameEncoder::with_dictionary`
// and `FrameDecoder` / `FrameDecoder::with_dictionary`.
//
// `Dict_ID` is supplied by the caller (the Ruby wrapper in `lib/rlz4.rb`
// derives it from `sha256(dict_bytes)[0..4]` interpreted little-endian).
// Doing the digest in Ruby keeps a hash crate out of the Rust extension's
// dependency tree.
#[magnus::wrap(class = "RLZ4::FrameCodec", free_immediately, size)]
struct FrameCodec {
    dict: Option<DictBound>,
}

struct DictBound {
    bytes: Vec<u8>,
    id: u32,
}

// Safety: FrameCodec is read-only after construction (dict bytes + id
// or no dict). No interior mutability, no thread-local refs. Shareable
// across Ractors.
unsafe impl Send for FrameCodec {}
unsafe impl Sync for FrameCodec {}

fn frame_codec_initialize(
    _ruby: &Ruby,
    rb_dict: Option<RString>,
    id: u32,
) -> Result<FrameCodec, Error> {
    let dict = rb_dict.map(|s| {
        // SAFETY: copy dict bytes into an owned Vec before any Ruby
        // allocation. Codec holds them for its lifetime.
        let bytes: Vec<u8> = unsafe { s.as_slice().to_vec() };
        s.freeze();
        DictBound { bytes, id }
    });
    Ok(FrameCodec { dict })
}

fn frame_codec_compress(
    ruby: &Ruby,
    rb_self: &FrameCodec,
    rb_input: RString,
) -> Result<RString, Error> {
    // SAFETY: borrow the RString's bytes directly. See rlz4_compress_frame.
    let input: &[u8] = unsafe { rb_input.as_slice() };
    let upper = get_maximum_output_size(input.len()) + 64;

    let compressed = match &rb_self.dict {
        None => {
            let mut encoder = FrameEncoder::new(Vec::with_capacity(upper));
            encoder.write_all(input).map_err(|e| {
                Error::new(
                    ruby.exception_runtime_error(),
                    format!("lz4 frame encoder write failed: {e}"),
                )
            })?;
            encoder.finish().map_err(|e| {
                Error::new(
                    ruby.exception_runtime_error(),
                    format!("lz4 frame encoder finish failed: {e}"),
                )
            })?
        }
        Some(d) => {
            let mut encoder = lz4_flex::frame::FrameEncoder::with_dictionary(
                Vec::with_capacity(upper),
                &d.bytes,
                d.id,
            );
            encoder.write_all(input).map_err(|e| {
                Error::new(
                    ruby.exception_runtime_error(),
                    format!("lz4 dict frame encode write failed: {e}"),
                )
            })?;
            encoder.finish().map_err(|e| {
                Error::new(
                    ruby.exception_runtime_error(),
                    format!("lz4 dict frame encode finish failed: {e}"),
                )
            })?
        }
    };

    Ok(ruby.str_from_slice(&compressed))
}

fn frame_codec_decompress(
    ruby: &Ruby,
    rb_self: &FrameCodec,
    rb_input: RString,
) -> Result<RString, Error> {
    // SAFETY: borrow the RString's bytes directly. See rlz4_compress_frame.
    let compressed: &[u8] = unsafe { rb_input.as_slice() };
    if compressed.len() < LZ4_FRAME_MAGIC.len() || compressed[..4] != LZ4_FRAME_MAGIC {
        return Err(Error::new(
            decompress_error(ruby),
            "lz4 frame decode failed: bad magic (input is not an LZ4 frame)",
        ));
    }

    let mut out = Vec::new();
    match &rb_self.dict {
        None => {
            let mut decoder = FrameDecoder::new(compressed);
            decoder.read_to_end(&mut out).map_err(|e| {
                Error::new(
                    decompress_error(ruby),
                    format!("lz4 frame decode failed: {e}"),
                )
            })?;
        }
        Some(d) => {
            let mut decoder = lz4_flex::frame::FrameDecoder::with_dictionary(
                compressed,
                &d.bytes,
                d.id,
            );
            decoder.read_to_end(&mut out).map_err(|e| {
                Error::new(
                    decompress_error(ruby),
                    format!("lz4 dict frame decode failed: {e}"),
                )
            })?;
        }
    }

    Ok(ruby.str_from_slice(&out))
}

fn frame_codec_size(rb_self: &FrameCodec) -> usize {
    rb_self.dict.as_ref().map_or(0, |d| d.bytes.len())
}

fn frame_codec_has_dict(rb_self: &FrameCodec) -> bool {
    rb_self.dict.is_some()
}

fn frame_codec_id(rb_self: &FrameCodec) -> Option<u32> {
    rb_self.dict.as_ref().map(|d| d.id)
}

// ---------- module init ----------

#[magnus::init]
fn init(ruby: &Ruby) -> Result<(), Error> {
    // Mark this extension as Ractor-safe for module-level and Dictionary
    // operations. BlockCodec uses a RefCell internally and must not cross
    // Ractor boundaries — the Ruby wrapper documents this and doesn't
    // implement `Ractor.make_shareable`.
    unsafe { rb_sys::rb_ext_ractor_safe(true) };

    let module = ruby.define_module("RLZ4")?;

    let decompress_error_class =
        module.define_error("DecompressError", ruby.exception_standard_error())?;
    DECOMPRESS_ERROR
        .set(Opaque::from(decompress_error_class))
        .unwrap_or_else(|_| panic!("init called more than once"));

    module.define_module_function("compress_bound", function!(rlz4_compress_bound, 1))?;

    // Block-format codec: stateful encoder (reusable scratch table, with
    // an optional dict-loaded pristine table for per-call memcpy restore).
    // Decompression is stateless but lives on the same class for
    // one-object-per-worker ergonomics.
    let codec_class = module.define_class("BlockCodec", ruby.class_object())?;
    codec_class.define_singleton_method("_native_new", function!(block_codec_new, 1))?;
    codec_class.define_method("size", method!(block_codec_size, 0))?;
    codec_class.define_method("has_dict?", method!(block_codec_has_dict, 0))?;
    codec_class.define_method("compress", method!(block_codec_compress, 1))?;
    codec_class.define_method("_decompress", method!(block_codec_decompress, 2))?;

    let frame_codec_class = module.define_class("FrameCodec", ruby.class_object())?;
    // Bound as `_native_new(dict_or_nil, id)`. Ruby's
    // `RLZ4::FrameCodec.new(dict: bytes)` computes the id and forwards
    // — see `lib/rlz4.rb`.
    frame_codec_class.define_singleton_method("_native_new", function!(frame_codec_initialize, 2))?;
    frame_codec_class.define_method("compress", method!(frame_codec_compress, 1))?;
    frame_codec_class.define_method("decompress", method!(frame_codec_decompress, 1))?;
    frame_codec_class.define_method("size", method!(frame_codec_size, 0))?;
    frame_codec_class.define_method("has_dict?", method!(frame_codec_has_dict, 0))?;
    frame_codec_class.define_method("id", method!(frame_codec_id, 0))?;

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

    #[test]
    fn block_table_round_trip() {
        let mut table = CompressTable::large();
        let msg = b"hello hello hello hello".to_vec();
        let mut out = vec![0u8; get_maximum_output_size(msg.len())];
        let n = compress_into_with_table(&msg, &mut out, &mut table).unwrap();

        let mut decoded = vec![0u8; msg.len()];
        let d = decompress_into(&out[..n], &mut decoded).unwrap();
        assert_eq!(&decoded[..d], msg.as_slice());
    }

    #[test]
    fn block_table_reuse_across_many_calls() {
        let mut table = CompressTable::large();
        for i in 0..100 {
            let msg = format!("payload number {i} ").repeat(10).into_bytes();
            let mut out = vec![0u8; get_maximum_output_size(msg.len())];
            let n = compress_into_with_table(&msg, &mut out, &mut table).unwrap();
            let mut decoded = vec![0u8; msg.len()];
            let d = decompress_into(&out[..n], &mut decoded).unwrap();
            assert_eq!(&decoded[..d], msg.as_slice());
        }
    }

    #[test]
    fn block_table_dict_round_trip() {
        let dict = b"common log prefix: ".to_vec();
        let mut table = CompressTable::large();
        let msg = b"common log prefix: event=login user=alice".to_vec();

        let mut out = vec![0u8; get_maximum_output_size(msg.len())];
        let n =
            compress_into_with_table_and_dict(&msg, &mut out, &mut table, &dict).unwrap();

        let mut decoded = vec![0u8; msg.len()];
        let d = decompress_into_with_dict(&out[..n], &mut decoded, &dict).unwrap();
        assert_eq!(&decoded[..d], msg.as_slice());

        // With-dict should be smaller than without on dict-sharing input.
        let mut no_dict = CompressTable::large();
        let mut out2 = vec![0u8; get_maximum_output_size(msg.len())];
        let n2 = compress_into_with_table(&msg, &mut out2, &mut no_dict).unwrap();
        assert!(n < n2, "dict compression should beat no-dict on shared-prefix input");
    }
}
