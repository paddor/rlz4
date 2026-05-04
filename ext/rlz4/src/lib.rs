use magnus::{
    exception::ExceptionClass,
    function, method,
    prelude::*,
    r_string::RString,
    value::Opaque,
    Error, Ruby,
};
use std::cell::RefCell;
use std::ptr;
use std::sync::OnceLock;

use lz4_sys::{
    LZ4F_VERSION,
    // block compress/decompress
    LZ4_compressBound, LZ4_compress_fast, LZ4_decompress_safe,
    LZ4_createStream, LZ4_freeStream, LZ4StreamEncode,
    // frame compress/decompress
    LZ4F_compressBound, LZ4F_compressBegin, LZ4F_compressUpdate, LZ4F_compressEnd,
    LZ4F_createCompressionContext, LZ4F_freeCompressionContext,
    LZ4F_createDecompressionContext, LZ4F_freeDecompressionContext,
    LZ4F_decompress, LZ4F_isError, LZ4F_getErrorName,
    LZ4FCompressionContext, LZ4FDecompressionContext,
    LZ4FPreferences, LZ4FFrameInfo, LZ4FDecompressOptions,
    BlockSize, BlockMode, ContentChecksum, FrameType, BlockChecksum,
    c_int,
};

// sizeof(LZ4_stream_t): union { char minStateSize[(1<<LZ4_MEMORY_USAGE)+32]; ... }
// LZ4_MEMORY_USAGE defaults to 14, so this is (16384 + 32) = 16416 bytes.
const LZ4_STREAM_SIZE: usize = (1 << 14) + 32;

// Functions present in liblz4 1.10.0 but not yet exposed by the lz4-sys crate.
extern "C" {
    fn LZ4_resetStream_fast(stream: *mut LZ4StreamEncode);
    fn LZ4_loadDict(stream: *mut LZ4StreamEncode, dict: *const u8, dict_size: c_int) -> c_int;
    fn LZ4_compress_fast_continue(
        stream: *mut LZ4StreamEncode,
        src: *const u8,
        dst: *mut u8,
        src_size: c_int,
        dst_capacity: c_int,
        acceleration: c_int,
    ) -> c_int;
    fn LZ4_decompress_safe_usingDict(
        src: *const u8,
        dst: *mut u8,
        src_size: c_int,
        dst_capacity: c_int,
        dict: *const u8,
        dict_size: c_int,
    ) -> c_int;
    // lz4 >= 1.9.4
    fn LZ4F_compressBegin_usingDict(
        ctx: LZ4FCompressionContext,
        dst: *mut u8,
        dst_capacity: usize,
        dict: *const u8,
        dict_size: usize,
        prefs: *const LZ4FPreferences,
    ) -> usize;
    // lz4 >= 1.9.4
    fn LZ4F_decompress_usingDict(
        ctx: LZ4FDecompressionContext,
        dst: *mut u8,
        dst_size_ptr: *mut usize,
        src: *const u8,
        src_size_ptr: *mut usize,
        dict: *const u8,
        dict_size: usize,
        opts: *const LZ4FDecompressOptions,
    ) -> usize;
}

const LZ4_FRAME_MAGIC: [u8; 4] = [0x04, 0x22, 0x4d, 0x18];

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
    Ok(unsafe { LZ4_compressBound(size as c_int) } as usize)
}

// ---------- module function: block_stream_size ----------
//
// Returns sizeof(LZ4_stream_t). Exposed so the Ruby test suite can compute
// the expected #size of a dict-mode BlockCodec without hardcoding the constant.

fn rlz4_block_stream_size(_ruby: &Ruby) -> usize {
    LZ4_STREAM_SIZE
}

// ---------- BlockCodec ----------
//
// No-dict codec: uses LZ4_compress_fast (stateless; stack-allocated hash
// table inside the C function). Ruby object owns no extra heap. #size = 0.
//
// Dict codec: allocates one LZ4StreamEncode via LZ4_createStream. Before
// each compress call, LZ4_resetStream_fast + LZ4_loadDict restore the
// dict-loaded state. #size = LZ4_STREAM_SIZE + dict.len().
//
// Decompression is always stateless per-block. Both compress and decompress
// live on the same class so callers hold one object per worker.
//
// Thread-local by construction (RefCell, not Send+Sync). A BlockCodec must
// not cross Ractor boundaries — send a new one instead.

struct EncodeStream(*mut LZ4StreamEncode);

// SAFETY: *mut LZ4StreamEncode is !Send by default. We guarantee exclusive
// access via RefCell (one borrow at a time, single-threaded Ruby GIL).
unsafe impl Send for EncodeStream {}

impl Drop for EncodeStream {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LZ4_freeStream(self.0) };
        }
    }
}

#[magnus::wrap(class = "RLZ4::BlockCodec", free_immediately, size)]
struct BlockCodec {
    stream: Option<RefCell<EncodeStream>>, // Some only when dict is set
    dict: Option<Vec<u8>>,
}

fn block_codec_new(_ruby: &Ruby, rb_dict: Option<RString>) -> Result<BlockCodec, Error> {
    match rb_dict {
        None => Ok(BlockCodec { stream: None, dict: None }),
        Some(rb_dict) => {
            // SAFETY: copy dict bytes before any Ruby allocation.
            let bytes: Vec<u8> = unsafe { rb_dict.as_slice().to_vec() };

            let raw = unsafe { LZ4_createStream() };
            if raw.is_null() {
                return Err(Error::new(
                    _ruby.exception_runtime_error(),
                    "LZ4_createStream allocation failed",
                ));
            }

            // Pre-load the dict so the pristine-state cost is paid once here,
            // not on the first compress call.
            unsafe { LZ4_loadDict(raw, bytes.as_ptr(), bytes.len() as c_int) };

            Ok(BlockCodec {
                stream: Some(RefCell::new(EncodeStream(raw))),
                dict: Some(bytes),
            })
        }
    }
}

fn block_codec_size(rb_self: &BlockCodec) -> usize {
    let stream_size = match &rb_self.stream {
        Some(_) => LZ4_STREAM_SIZE,
        None => 0,
    };
    stream_size + rb_self.dict.as_ref().map_or(0, |d| d.len())
}

fn block_codec_has_dict(rb_self: &BlockCodec) -> bool {
    rb_self.dict.is_some()
}

fn block_codec_compress(
    ruby: &Ruby,
    rb_self: &BlockCodec,
    rb_input: RString,
) -> Result<RString, Error> {
    // SAFETY: rb_input is stack-pinned; the C compression functions perform no
    // Ruby callbacks or GC-triggering allocations while the input slice is
    // live. str_from_slice happens after.
    let input: &[u8] = unsafe { rb_input.as_slice() };

    let upper = unsafe { LZ4_compressBound(input.len() as c_int) as usize };
    let mut out = vec![0u8; upper];

    let compressed_len: c_int = match (&rb_self.stream, &rb_self.dict) {
        (None, None) => unsafe {
            LZ4_compress_fast(
                input.as_ptr() as *const _,
                out.as_mut_ptr() as *mut _,
                input.len() as c_int,
                upper as c_int,
                1,
            )
        },
        (Some(stream_cell), Some(dict)) => {
            let stream = stream_cell.borrow_mut();
            unsafe {
                // Restore stream to the dict-loaded state before each call.
                LZ4_resetStream_fast(stream.0);
                LZ4_loadDict(stream.0, dict.as_ptr(), dict.len() as c_int);
                LZ4_compress_fast_continue(
                    stream.0,
                    input.as_ptr(),
                    out.as_mut_ptr(),
                    input.len() as c_int,
                    upper as c_int,
                    1,
                )
            }
        }
        _ => unreachable!("stream and dict are always both Some or both None"),
    };

    if compressed_len <= 0 {
        return Err(Error::new(
            ruby.exception_runtime_error(),
            "lz4 block compress failed",
        ));
    }

    out.truncate(compressed_len as usize);
    Ok(ruby.str_from_slice(&out))
}

fn block_codec_decompress(
    ruby: &Ruby,
    rb_self: &BlockCodec,
    rb_input: RString,
    decompressed_size: usize,
) -> Result<RString, Error> {
    // SAFETY: same as compress. Decoder is pure C, no Ruby callbacks.
    let compressed: &[u8] = unsafe { rb_input.as_slice() };

    let mut out = vec![0u8; decompressed_size];

    let actual_len: c_int = match &rb_self.dict {
        None => unsafe {
            LZ4_decompress_safe(
                compressed.as_ptr() as *const _,
                out.as_mut_ptr() as *mut _,
                compressed.len() as c_int,
                decompressed_size as c_int,
            )
        },
        Some(dict) => unsafe {
            LZ4_decompress_safe_usingDict(
                compressed.as_ptr(),
                out.as_mut_ptr(),
                compressed.len() as c_int,
                decompressed_size as c_int,
                dict.as_ptr(),
                dict.len() as c_int,
            )
        },
    };

    if actual_len < 0 {
        return Err(Error::new(
            decompress_error(ruby),
            "lz4 block decode failed",
        ));
    }

    out.truncate(actual_len as usize);
    Ok(ruby.str_from_slice(&out))
}

// ---------- FrameCodec ----------
//
// One-shot compress/decompress using the LZ4F frame API. Contexts are
// created and freed per operation so FrameCodec holds no mutable state
// and is shareable across Ractors.
//
// Block mode: Linked. In Linked mode LZ4F_compressBegin_usingDict loads
// the dict as initial stream history before the first block, so the block
// compressor can back-reference into dict bytes. Independent mode would
// discard the raw dict bytes before each block (a known liblz4 limitation
// with the _usingDict raw-bytes API; _usingCDict avoids it but changes the
// dict-id derivation).

#[magnus::wrap(class = "RLZ4::FrameCodec", free_immediately, size)]
struct FrameCodec {
    dict: Option<DictBound>,
}

struct DictBound {
    bytes: Vec<u8>,
    id: u32,
}

unsafe impl Send for FrameCodec {}
unsafe impl Sync for FrameCodec {}

fn frame_codec_initialize(
    _ruby: &Ruby,
    rb_dict: Option<RString>,
    id: u32,
) -> Result<FrameCodec, Error> {
    let dict = rb_dict.map(|s| {
        // SAFETY: copy dict bytes before any Ruby allocation.
        let bytes: Vec<u8> = unsafe { s.as_slice().to_vec() };
        s.freeze();
        DictBound { bytes, id }
    });
    Ok(FrameCodec { dict })
}

fn lz4f_error(code: usize) -> String {
    let name = unsafe { LZ4F_getErrorName(code) };
    if name.is_null() {
        return format!("lz4f error {code}");
    }
    unsafe { std::ffi::CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned()
}

fn default_prefs(dict_id: u32) -> LZ4FPreferences {
    LZ4FPreferences {
        frame_info: LZ4FFrameInfo {
            block_size_id: BlockSize::Default,
            // Linked mode: the dict is treated as the initial stream history,
            // so the first (and often only) block can back-reference into it.
            // Independent mode + raw-bytes dict would reset the hash table
            // before each block, discarding the dict (liblz4 limitation).
            block_mode: BlockMode::Linked,
            content_checksum_flag: ContentChecksum::NoChecksum,
            frame_type: FrameType::Frame,
            content_size: 0,
            dict_id,
            block_checksum_flag: BlockChecksum::NoBlockChecksum,
        },
        compression_level: 0,
        auto_flush: 0,
        favor_dec_speed: 0,
        reserved: [0; 3],
    }
}

fn zero_frame_info() -> LZ4FFrameInfo {
    LZ4FFrameInfo {
        block_size_id: BlockSize::Default,
        block_mode: BlockMode::Independent,
        content_checksum_flag: ContentChecksum::NoChecksum,
        frame_type: FrameType::Frame,
        content_size: 0,
        dict_id: 0,
        block_checksum_flag: BlockChecksum::NoBlockChecksum,
    }
}

fn create_dctx(ruby: &Ruby) -> Result<LZ4FDecompressionContext, Error> {
    let mut ctx = LZ4FDecompressionContext(ptr::null_mut());
    let err = unsafe { LZ4F_createDecompressionContext(&mut ctx, LZ4F_VERSION) };
    if unsafe { LZ4F_isError(err) } != 0 {
        return Err(Error::new(
            ruby.exception_runtime_error(),
            format!("LZ4F_createDecompressionContext: {}", lz4f_error(err)),
        ));
    }
    Ok(ctx)
}

fn frame_codec_compress(
    ruby: &Ruby,
    rb_self: &FrameCodec,
    rb_input: RString,
) -> Result<RString, Error> {
    // SAFETY: rb_input is stack-pinned; all LZ4F calls are pure C with no
    // Ruby callbacks. str_from_slice happens after input is no longer live.
    let input: &[u8] = unsafe { rb_input.as_slice() };

    let prefs = default_prefs(rb_self.dict.as_ref().map_or(0, |d| d.id));
    let data_bound = unsafe { LZ4F_compressBound(input.len(), &prefs) };
    let capacity = data_bound + 64;
    let mut out = vec![0u8; capacity];
    let mut pos: usize = 0;

    let mut ctx = LZ4FCompressionContext(ptr::null_mut());
    let err = unsafe { LZ4F_createCompressionContext(&mut ctx, LZ4F_VERSION) };
    if unsafe { LZ4F_isError(err) } != 0 {
        return Err(Error::new(
            ruby.exception_runtime_error(),
            format!("LZ4F_createCompressionContext: {}", lz4f_error(err)),
        ));
    }

    let result = (|| -> Result<usize, String> {
        let n = match &rb_self.dict {
            None => unsafe {
                LZ4F_compressBegin(ctx, out.as_mut_ptr().add(pos), capacity - pos, &prefs)
            },
            Some(d) => unsafe {
                LZ4F_compressBegin_usingDict(
                    ctx,
                    out.as_mut_ptr().add(pos),
                    capacity - pos,
                    d.bytes.as_ptr(),
                    d.bytes.len(),
                    &prefs,
                )
            },
        };
        if unsafe { LZ4F_isError(n) } != 0 {
            return Err(format!("LZ4F_compressBegin: {}", lz4f_error(n)));
        }
        pos += n;

        let n = unsafe {
            LZ4F_compressUpdate(
                ctx,
                out.as_mut_ptr().add(pos),
                capacity - pos,
                input.as_ptr(),
                input.len(),
                ptr::null(),
            )
        };
        if unsafe { LZ4F_isError(n) } != 0 {
            return Err(format!("LZ4F_compressUpdate: {}", lz4f_error(n)));
        }
        pos += n;

        let n = unsafe {
            LZ4F_compressEnd(
                ctx,
                out.as_mut_ptr().add(pos),
                capacity - pos,
                ptr::null(),
            )
        };
        if unsafe { LZ4F_isError(n) } != 0 {
            return Err(format!("LZ4F_compressEnd: {}", lz4f_error(n)));
        }
        pos += n;
        Ok(pos)
    })();

    unsafe { LZ4F_freeCompressionContext(ctx) };

    match result {
        Err(msg) => Err(Error::new(ruby.exception_runtime_error(), msg)),
        Ok(written) => {
            out.truncate(written);
            Ok(ruby.str_from_slice(&out))
        }
    }
}

fn frame_codec_decompress(
    ruby: &Ruby,
    rb_self: &FrameCodec,
    rb_input: RString,
) -> Result<RString, Error> {
    // SAFETY: rb_input is stack-pinned; LZ4F calls are pure C.
    let compressed: &[u8] = unsafe { rb_input.as_slice() };

    if compressed.len() < 4 || compressed[..4] != LZ4_FRAME_MAGIC {
        return Err(Error::new(
            decompress_error(ruby),
            "lz4 frame decode failed: bad magic (input is not an LZ4 frame)",
        ));
    }

    // When we have a dict, use a temporary context to parse the frame header
    // (LZ4F_getFrameInfo advances the context's stage past dstage_init), then
    // use a fresh context for the actual decompress so LZ4F_decompress_usingDict
    // sees dstage_init and correctly installs the dict.
    if let Some(d) = &rb_self.dict {
        let temp_ctx = create_dctx(ruby)?;
        let mut frame_info = zero_frame_info();
        let mut dummy = compressed.len();
        let ret = unsafe {
            lz4_sys::LZ4F_getFrameInfo(temp_ctx, &mut frame_info, compressed.as_ptr(), &mut dummy)
        };
        unsafe { LZ4F_freeDecompressionContext(temp_ctx) };

        if unsafe { LZ4F_isError(ret) } != 0 {
            return Err(Error::new(
                decompress_error(ruby),
                format!("lz4 frame header error: {}", lz4f_error(ret)),
            ));
        }

        if frame_info.dict_id != 0 && frame_info.dict_id != d.id {
            return Err(Error::new(
                decompress_error(ruby),
                format!(
                    "lz4 frame dict_id mismatch: frame={:#010x} codec={:#010x}",
                    frame_info.dict_id, d.id
                ),
            ));
        }
    }

    // Fresh context for actual decompression.
    let ctx = create_dctx(ruby)?;

    // Pass the full compressed buffer (including header) to the loop.
    // LZ4F_decompress_usingDict sets the dict before parsing the header
    // (dstage_init check), so the dict is available for the first block.
    let result = frame_decompress_loop(ctx, compressed, rb_self.dict.as_ref());

    unsafe { LZ4F_freeDecompressionContext(ctx) };

    match result {
        Err(msg) => Err(Error::new(decompress_error(ruby), msg)),
        Ok(out) => Ok(ruby.str_from_slice(&out)),
    }
}

fn frame_decompress_loop(
    ctx: LZ4FDecompressionContext,
    compressed: &[u8],
    dict: Option<&DictBound>,
) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut src_pos = 0usize;
    let mut chunk = vec![0u8; 65536];
    let mut complete = false;

    loop {
        let remaining = compressed.len() - src_pos;
        if remaining == 0 {
            break;
        }

        let mut dst_written = chunk.len();
        let mut src_consumed = remaining;

        let ret = match dict {
            None => unsafe {
                LZ4F_decompress(
                    ctx,
                    chunk.as_mut_ptr(),
                    &mut dst_written,
                    compressed.as_ptr().add(src_pos),
                    &mut src_consumed,
                    ptr::null(),
                )
            },
            Some(d) => unsafe {
                LZ4F_decompress_usingDict(
                    ctx,
                    chunk.as_mut_ptr(),
                    &mut dst_written as *mut usize,
                    compressed.as_ptr().add(src_pos),
                    &mut src_consumed as *mut usize,
                    d.bytes.as_ptr(),
                    d.bytes.len(),
                    ptr::null(),
                )
            },
        };

        src_pos += src_consumed;
        out.extend_from_slice(&chunk[..dst_written]);

        if unsafe { LZ4F_isError(ret) } != 0 {
            return Err(format!("lz4 frame decode failed: {}", lz4f_error(ret)));
        }
        if ret == 0 {
            complete = true;
            break;
        }
        // Guard against a degenerate case where the C library makes no progress.
        if src_consumed == 0 && dst_written == 0 {
            break;
        }
    }

    if !complete {
        return Err("lz4 frame decode failed: truncated or incomplete frame".to_string());
    }

    Ok(out)
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
    unsafe { rb_sys::rb_ext_ractor_safe(true) };

    let module = ruby.define_module("RLZ4")?;

    let decompress_error_class =
        module.define_error("DecompressError", ruby.exception_standard_error())?;
    DECOMPRESS_ERROR
        .set(Opaque::from(decompress_error_class))
        .unwrap_or_else(|_| panic!("init called more than once"));

    module.define_module_function("compress_bound", function!(rlz4_compress_bound, 1))?;
    module.define_module_function("block_stream_size", function!(rlz4_block_stream_size, 0))?;

    let codec_class = module.define_class("BlockCodec", ruby.class_object())?;
    codec_class.define_singleton_method("_native_new", function!(block_codec_new, 1))?;
    codec_class.define_method("size", method!(block_codec_size, 0))?;
    codec_class.define_method("has_dict?", method!(block_codec_has_dict, 0))?;
    codec_class.define_method("compress", method!(block_codec_compress, 1))?;
    codec_class.define_method("_decompress", method!(block_codec_decompress, 2))?;

    let frame_codec_class = module.define_class("FrameCodec", ruby.class_object())?;
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

    fn lz4_block_compress(input: &[u8]) -> Vec<u8> {
        let upper = unsafe { LZ4_compressBound(input.len() as c_int) as usize };
        let mut out = vec![0u8; upper];
        let n = unsafe {
            LZ4_compress_fast(
                input.as_ptr() as *const _,
                out.as_mut_ptr() as *mut _,
                input.len() as c_int,
                upper as c_int,
                1,
            )
        };
        assert!(n > 0);
        out.truncate(n as usize);
        out
    }

    fn lz4_block_decompress(compressed: &[u8], original_len: usize) -> Vec<u8> {
        let mut out = vec![0u8; original_len];
        let n = unsafe {
            LZ4_decompress_safe(
                compressed.as_ptr() as *const _,
                out.as_mut_ptr() as *mut _,
                compressed.len() as c_int,
                original_len as c_int,
            )
        };
        assert!(n >= 0);
        out.truncate(n as usize);
        out
    }

    fn lz4_block_compress_dict(input: &[u8], dict: &[u8]) -> Vec<u8> {
        let upper = unsafe { LZ4_compressBound(input.len() as c_int) as usize };
        let mut out = vec![0u8; upper];
        let stream = unsafe { LZ4_createStream() };
        assert!(!stream.is_null());
        unsafe { LZ4_loadDict(stream, dict.as_ptr(), dict.len() as c_int) };
        let n = unsafe {
            LZ4_compress_fast_continue(
                stream,
                input.as_ptr(),
                out.as_mut_ptr(),
                input.len() as c_int,
                upper as c_int,
                1,
            )
        };
        unsafe { LZ4_freeStream(stream) };
        assert!(n > 0);
        out.truncate(n as usize);
        out
    }

    fn lz4_block_decompress_dict(compressed: &[u8], original_len: usize, dict: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; original_len];
        let n = unsafe {
            LZ4_decompress_safe_usingDict(
                compressed.as_ptr(),
                out.as_mut_ptr(),
                compressed.len() as c_int,
                original_len as c_int,
                dict.as_ptr(),
                dict.len() as c_int,
            )
        };
        assert!(n >= 0);
        out.truncate(n as usize);
        out
    }

    #[test]
    fn block_round_trip() {
        let data = b"hello hello hello hello".to_vec();
        let ct = lz4_block_compress(&data);
        let pt = lz4_block_decompress(&ct, data.len());
        assert_eq!(pt, data);
    }

    #[test]
    fn block_reuse_across_many_calls() {
        for i in 0..100 {
            let msg = format!("payload number {i} ").repeat(10).into_bytes();
            let ct = lz4_block_compress(&msg);
            let pt = lz4_block_decompress(&ct, msg.len());
            assert_eq!(pt, msg);
        }
    }

    #[test]
    fn block_dict_round_trip() {
        let dict = b"common log prefix: ".to_vec();
        let msg = b"common log prefix: event=login user=alice".to_vec();

        let ct_dict = lz4_block_compress_dict(&msg, &dict);
        let pt = lz4_block_decompress_dict(&ct_dict, msg.len(), &dict);
        assert_eq!(pt, msg);

        let ct_plain = lz4_block_compress(&msg);
        assert!(
            ct_dict.len() < ct_plain.len(),
            "dict compression should beat no-dict on shared-prefix input"
        );
    }

    fn frame_compress(input: &[u8], dict: Option<(&[u8], u32)>) -> Vec<u8> {
        let prefs = default_prefs(dict.map_or(0, |(_, id)| id));
        let data_bound = unsafe { LZ4F_compressBound(input.len(), &prefs) };
        let capacity = data_bound + 64;
        let mut out = vec![0u8; capacity];
        let mut pos = 0usize;

        let mut ctx = LZ4FCompressionContext(ptr::null_mut());
        let err = unsafe { LZ4F_createCompressionContext(&mut ctx, LZ4F_VERSION) };
        assert_eq!(unsafe { LZ4F_isError(err) }, 0);

        let n = match dict {
            None => unsafe {
                LZ4F_compressBegin(ctx, out.as_mut_ptr().add(pos), capacity - pos, &prefs)
            },
            Some((d, _)) => unsafe {
                LZ4F_compressBegin_usingDict(
                    ctx,
                    out.as_mut_ptr().add(pos),
                    capacity - pos,
                    d.as_ptr(),
                    d.len(),
                    &prefs,
                )
            },
        };
        assert_eq!(unsafe { LZ4F_isError(n) }, 0);
        pos += n;

        let n = unsafe {
            LZ4F_compressUpdate(
                ctx,
                out.as_mut_ptr().add(pos),
                capacity - pos,
                input.as_ptr(),
                input.len(),
                ptr::null(),
            )
        };
        assert_eq!(unsafe { LZ4F_isError(n) }, 0);
        pos += n;

        let n = unsafe {
            LZ4F_compressEnd(ctx, out.as_mut_ptr().add(pos), capacity - pos, ptr::null())
        };
        assert_eq!(unsafe { LZ4F_isError(n) }, 0);
        pos += n;

        unsafe { LZ4F_freeCompressionContext(ctx) };
        out.truncate(pos);
        out
    }

    fn frame_decompress(compressed: &[u8], dict: Option<&[u8]>) -> Vec<u8> {
        let mut ctx = LZ4FDecompressionContext(ptr::null_mut());
        let err = unsafe { LZ4F_createDecompressionContext(&mut ctx, LZ4F_VERSION) };
        assert_eq!(unsafe { LZ4F_isError(err) }, 0);

        let d = dict.map(|b| DictBound { bytes: b.to_vec(), id: 0 });
        let out = frame_decompress_loop(ctx, compressed, d.as_ref()).unwrap();

        unsafe { LZ4F_freeDecompressionContext(ctx) };
        out
    }

    #[test]
    fn frame_round_trip() {
        let data = b"the quick brown fox jumps over the lazy dog ".repeat(100);
        let ct = frame_compress(&data, None);
        assert!(ct.len() < data.len(), "should compress repetitive input");
        assert_eq!(&ct[..4], &LZ4_FRAME_MAGIC);
        let pt = frame_decompress(&ct, None);
        assert_eq!(pt, data);
    }

    #[test]
    fn frame_empty_round_trip() {
        let ct = frame_compress(b"", None);
        let pt = frame_decompress(&ct, None);
        assert!(pt.is_empty());
    }

    #[test]
    fn frame_garbage_fails() {
        let garbage = vec![0xFFu8; 32];
        let mut ctx = LZ4FDecompressionContext(ptr::null_mut());
        unsafe { LZ4F_createDecompressionContext(&mut ctx, LZ4F_VERSION) };
        let result = frame_decompress_loop(ctx, &garbage, None);
        unsafe { LZ4F_freeDecompressionContext(ctx) };
        assert!(result.is_err());
    }

    #[test]
    fn frame_dict_round_trip() {
        let dict = b"JSON schema version 1 field ".repeat(4);
        let id: u32 = 0xDEAD_BEEF;
        let msg = b"JSON schema version 1 field name=hello value=world".to_vec();

        let ct = frame_compress(&msg, Some((&dict, id)));
        assert_eq!(&ct[..4], &LZ4_FRAME_MAGIC);

        let pt = frame_decompress(&ct, Some(&dict));
        assert_eq!(pt, msg);
    }

    #[test]
    fn frame_dict_id_in_header() {
        let dict = b"common prefix AAA ".repeat(4);
        let id: u32 = 0xAAAA_AAAA;
        let msg = b"common prefix AAA : the payload";

        let ct = frame_compress(msg, Some((&dict, id)));

        let mut ctx = LZ4FDecompressionContext(ptr::null_mut());
        unsafe { LZ4F_createDecompressionContext(&mut ctx, LZ4F_VERSION) };

        let mut frame_info = zero_frame_info();
        let mut src_size = ct.len();
        let ret = unsafe {
            lz4_sys::LZ4F_getFrameInfo(ctx, &mut frame_info, ct.as_ptr(), &mut src_size)
        };
        unsafe { LZ4F_freeDecompressionContext(ctx) };

        assert_eq!(unsafe { LZ4F_isError(ret) }, 0, "LZ4F_getFrameInfo failed");
        assert_eq!(frame_info.dict_id, id, "dict_id not written into frame header");
    }

    #[test]
    fn frame_truncated_fails() {
        let data = b"some data that should compress nicely ".repeat(10);
        let ct = frame_compress(&data, None);
        let truncated = &ct[..ct.len() / 2];

        let mut ctx = LZ4FDecompressionContext(ptr::null_mut());
        unsafe { LZ4F_createDecompressionContext(&mut ctx, LZ4F_VERSION) };
        let result = frame_decompress_loop(ctx, truncated, None);
        unsafe { LZ4F_freeDecompressionContext(ctx) };

        assert!(result.is_err(), "truncated frame should return an error");
    }
}
