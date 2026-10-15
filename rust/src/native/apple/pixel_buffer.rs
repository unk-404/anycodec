//! `CVPixelBuffer` / `CMBlockBuffer` / `CMSampleBuffer` plumbing.
//!
//! VideoToolbox never sees "our" frames: it takes `CVImageBuffer`s in and hands
//! `CMSampleBuffer`s (or `CVImageBuffer`s) back. This module is the one place
//! that translates between AnyCodec's [`VideoFrame`] / [`Packet`] model and
//! CoreMedia's, so both session types can stay about *sessions*.
//!
//! ## The pixel format table
//!
//! [`pixel_format_type`] maps every [`PixelFormat`] that CoreVideo can carry
//! without reinterpreting the bytes. `Nv21` is the one layout CoreVideo has no
//! equivalent for 鈥?`kCVPixelFormatType_420YpCbCr8BiPlanarFullRange` also has its
//! chroma components in Cb,Cr order 鈥?so mapping `Nv21` onto it would silently
//! swap U and V. It maps to `None` and the session refuses it instead.
//!
//! `P010` *does* map, onto the two-plane
//! `kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange`. AnyCodec models 10-bit
//! 4:2:0 as three planes (luma, Cb, Cr) and CoreVideo models it as two (luma,
//! interleaved CbCr), so [`fill_buffer`] interleaves the chroma planes on the way
//! in and [`read_buffer`] de-interleaves them on the way out. That conversion is
//! lossless: a 10-bit sample keeps all ten bits in a little-endian 16-bit word,
//! and no value is scaled or clamped.
//!
//! [`Packet`]: crate::media::packet::Packet

use std::ffi::{c_char, c_int, c_void};
use std::ptr::NonNull;

use objc2_core_foundation::{CFRetained, CFType};
use objc2_core_media::{
    kCMBlockBufferAssureMemoryNowFlag, CMBlockBuffer, CMFormatDescription, CMSampleBuffer,
    CMSampleTimingInfo, CMVideoFormatDescription, CMVideoFormatDescriptionCreate,
    CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets,
};
use objc2_core_video::{
    kCVReturnSuccess, kCVPixelFormatType_32BGRA, kCVPixelFormatType_32RGBA,
    kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, kCVPixelFormatType_420YpCbCr8Planar,
    kCVPixelFormatType_422YpCbCr8_yuvs, CVImageBuffer, CVPixelBuffer, CVPixelBufferCreate,
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRow,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetPlaneCount, CVPixelBufferGetWidth,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};

use crate::error::{Error, Result};
use crate::media::buffer::Buffer;
use crate::media::codec::{Codec, VideoCodec};
use crate::media::format::PixelFormat;
use crate::media::frame::{Plane, VideoFrame};
use crate::media::packet::Packet;
use crate::native::bitstream;

use super::{four_char_code, ms_from_time, os_status_text, time_from_ms};

/// The `kCVPixelFormatType_*` a frame of `format` is carried in, or `None` when
/// CoreVideo has no layout that matches it.
///
/// Total over every [`PixelFormat`]: the only `None` is `Nv21`, and the reason is
/// in the module documentation.
#[must_use]
pub fn pixel_format_type(format: PixelFormat) -> Option<u32> {
    Some(match format {
        PixelFormat::I420 => kCVPixelFormatType_420YpCbCr8Planar,
        PixelFormat::Nv12 => kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
        PixelFormat::Nv21 => return None,
        PixelFormat::Rgba8 => kCVPixelFormatType_32RGBA,
        PixelFormat::Bgra8 => kCVPixelFormatType_32BGRA,
        PixelFormat::Yuy2 => kCVPixelFormatType_422YpCbCr8_yuvs,
        PixelFormat::P010 => kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange,
    })
}

/// The AnyCodec layout a `CVPixelBuffer` of `pixel_format_type` is read as.
///
/// The inverse of [`pixel_format_type`] on every value that function produces.
/// Every other `kCVPixelFormatType_*` 鈥?the full-range variants, the 4:2:2
/// biplanar variants, the packed 10-bit ones, ... 鈥?returns `None`, and the
/// session then fails with a message naming the code rather than guessing at a
/// colour range or a chroma order.
#[must_use]
pub fn pixel_format_from_type(pixel_format_type: u32) -> Option<PixelFormat> {
    if pixel_format_type == kCVPixelFormatType_420YpCbCr8Planar {
        Some(PixelFormat::I420)
    } else if pixel_format_type == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange {
        Some(PixelFormat::Nv12)
    } else if pixel_format_type == kCVPixelFormatType_32RGBA {
        Some(PixelFormat::Rgba8)
    } else if pixel_format_type == kCVPixelFormatType_32BGRA {
        Some(PixelFormat::Bgra8)
    } else if pixel_format_type == kCVPixelFormatType_422YpCbCr8_yuvs {
        Some(PixelFormat::Yuy2)
    } else if pixel_format_type == kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange {
        Some(PixelFormat::P010)
    } else {
        None
    }
}

/// `true` when CoreVideo can carry `format`, i.e. when a session may accept it.
#[must_use]
pub fn is_supported(format: PixelFormat) -> bool {
    pixel_format_type(format).is_some()
}

// ---------------------------------------------------------------------------
// Filling the buffers VideoToolbox takes
// ---------------------------------------------------------------------------

/// Allocates an empty `CVPixelBuffer` of `width` 脳 `height` in `format`.
///
/// The buffer is allocated by CoreVideo and handed straight to the encoder. No
/// `CVPixelBufferPool` is used, so every frame pays one allocation; that is
/// honest but not free, and a pooling encoder would reuse a handful of buffers.
pub fn create_buffer(
    format: PixelFormat,
    width: u32,
    height: u32,
) -> Result<CFRetained<CVPixelBuffer>> {
    let Some(pixel_format_type) = pixel_format_type(format) else {
        return Err(Error::unsupported(format!(
            "CoreVideo has no pixel format for {} (its chroma planes are in V,U order)",
            format.name()
        )));
    };
    if width == 0 || height == 0 {
        return Err(Error::invalid_argument("a pixel buffer needs a non-zero size"));
    }
    let mut buffer: *mut CVPixelBuffer = std::ptr::null_mut();
    let Some(out) = NonNull::new(&mut buffer) else {
        return Err(Error::internal("the pixel buffer out-parameter cannot be null"));
    };
    // SAFETY: `CVPixelBufferCreate` writes a +1 reference into `out` when it
    // succeeds; `out` is a valid local and the attributes are optional.
    let status = unsafe {
        CVPixelBufferCreate(None, width as usize, height as usize, pixel_format_type, None, out)
    };
    if status != kCVReturnSuccess {
        return Err(Error::backend(format!(
            "CVPixelBufferCreate({}x{}, '{}') failed: {}",
            width,
            height,
            four_char_code(pixel_format_type as i32),
            os_status_text(status)
        )));
    }
    let Some(pointer) = NonNull::new(buffer) else {
        return Err(Error::backend("CVPixelBufferCreate reported success but returned no buffer"));
    };
    // SAFETY: `CVPixelBufferCreate` follows the Create rule, so the returned
    // reference carries +1 and `CFRetained` takes ownership of exactly that.
    Ok(unsafe { CFRetained::from_raw(pointer) })
}

/// Copies `frame`'s planes into `buffer`.
///
/// The plane order and the chroma interleaving follow the *buffer's* pixel
/// format, so this is also where AnyCodec's three-plane 10-bit layout becomes
/// CoreVideo's two-plane one.
pub fn fill_buffer(buffer: &CVPixelBuffer, frame: &VideoFrame) -> Result<()> {
    let buffer_width = CVPixelBufferGetWidth(buffer);
    let buffer_height = CVPixelBufferGetHeight(buffer);
    if buffer_width != frame.width as usize || buffer_height != frame.height as usize {
        return Err(Error::invalid_argument(format!(
            "the pixel buffer is {}x{} but the frame is {}x{}",
            buffer_width, buffer_height, frame.width, frame.height
        )));
    }
    let kind = CVPixelBufferGetPixelFormatType(buffer);
    let Some(buffer_format) = pixel_format_from_type(kind) else {
        return Err(Error::unsupported(format!(
            "cannot fill a CVPixelBuffer of type '{}'",
            four_char_code(kind as i32)
        )));
    };
    if buffer_format != frame.format {
        return Err(Error::invalid_argument(format!(
            "the pixel buffer holds {} but the frame is {}",
            buffer_format.name(),
            frame.format.name()
        )));
    }

    // SAFETY: locking a live pixel buffer for writing is the documented
    // precondition for touching the plane base addresses below.
    let lock = unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags(0)) };
    if lock != kCVReturnSuccess {
        return Err(Error::backend(format!(
            "CVPixelBufferLockBaseAddress failed: {}",
            os_status_text(lock)
        )));
    }
    let written = write_planes(buffer, frame);
    // SAFETY: the buffer was locked above with the same flags, and must be
    // unlocked whether or not the copy succeeded.
    let unlock = unsafe { CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags(0)) };
    written?;
    if unlock != kCVReturnSuccess {
        return Err(Error::backend(format!(
            "CVPixelBufferUnlockBaseAddress failed: {}",
            os_status_text(unlock)
        )));
    }
    Ok(())
}

fn write_planes(buffer: &CVPixelBuffer, frame: &VideoFrame) -> Result<()> {
    let planes = CVPixelBufferGetPlaneCount(buffer);
    if planes == 0 {
        // A packed single-plane format: copy row by row, honouring CoreVideo's
        // row stride, which need not equal the frame's.
        let stride = CVPixelBufferGetBytesPerRow(buffer);
        let height = CVPixelBufferGetHeight(buffer);
        let base = CVPixelBufferGetBaseAddress(buffer);
        if base.is_null() || stride == 0 {
            return Err(Error::backend("the packed pixel buffer has no accessible memory"));
        }
        let source = frame
            .planes
            .first()
            .ok_or_else(|| Error::invalid_argument("the frame carries no plane"))?;
        let row_bytes = frame.format.default_stride(frame.width, 0)?;
        copy_rows(base, stride, source, row_bytes, height)
    } else if frame.format == PixelFormat::P010 {
        write_planar_10(buffer, frame)
    } else {
        for index in 0..planes {
            let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, index);
            let height = CVPixelBufferGetHeightOfPlane(buffer, index);
            let base = CVPixelBufferGetBaseAddressOfPlane(buffer, index);
            if base.is_null() || stride == 0 {
                return Err(Error::backend(format!(
                    "plane {index} of the pixel buffer is not accessible"
                )));
            }
            let source = frame.planes.get(index).ok_or_else(|| {
                Error::invalid_argument(format!("the frame has no plane {index}"))
            })?;
            let (plane_width, _) = frame.format.plane_size(frame.width, frame.height, index)?;
            let row_bytes = frame.format.default_stride(plane_width, 0)?;
            copy_rows(base, stride, source, row_bytes, height)?;
        }
        Ok(())
    }
}

/// Writes a three-plane 10-bit 4:2:0 frame into CoreVideo's two-plane layout.
fn write_planar_10(buffer: &CVPixelBuffer, frame: &VideoFrame) -> Result<()> {
    let luma_stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0);
    let luma_height = CVPixelBufferGetHeightOfPlane(buffer, 0);
    let luma = CVPixelBufferGetBaseAddressOfPlane(buffer, 0);
    if luma.is_null() || luma_stride == 0 {
        return Err(Error::backend("the 10-bit luma plane is not accessible"));
    }
    let source = frame
        .planes
        .first()
        .ok_or_else(|| Error::invalid_argument("the frame has no luma plane"))?;
    let row_bytes = frame.format.default_stride(frame.width, 0)?;
    copy_rows(luma, luma_stride, source, row_bytes, luma_height)?;

    let chroma_stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1);
    let chroma_height = CVPixelBufferGetHeightOfPlane(buffer, 1);
    let chroma = CVPixelBufferGetBaseAddressOfPlane(buffer, 1);
    if chroma.is_null() || chroma_stride == 0 {
        return Err(Error::backend("the 10-bit chroma plane is not accessible"));
    }
    let cb = frame
        .planes
        .get(1)
        .ok_or_else(|| Error::invalid_argument("the frame has no Cb plane"))?;
    let cr = frame
        .planes
        .get(2)
        .ok_or_else(|| Error::invalid_argument("the frame has no Cr plane"))?;
    let samples = frame.width.div_ceil(2) as usize;
    interleave_chroma_16(chroma, chroma_stride, cb, cr, samples, chroma_height)
}

/// Copies `height` rows of `row_bytes` bytes from a frame plane into a CoreVideo
/// plane whose rows are `stride` bytes apart.
fn copy_rows(
    destination: *mut c_void,
    stride: usize,
    source: &Plane,
    row_bytes: usize,
    height: usize,
) -> Result<()> {
    if row_bytes > stride {
        return Err(Error::backend(format!(
            "the pixel buffer row is {stride} bytes but {row_bytes} bytes are needed"
        )));
    }
    for row in 0..height {
        let source_row = source.row(row, row_bytes)?;
        // SAFETY: the plane is locked and `stride * height` bytes large, and
        // `row < height` with `row_bytes <= stride`, so the write stays inside it.
        unsafe {
            let destination = destination.cast::<u8>().add(row * stride);
            std::ptr::copy_nonoverlapping(source_row.as_ptr(), destination, row_bytes);
        }
    }
    Ok(())
}

/// Interleaves the Cb and Cr planes of a 10-bit 4:2:0 frame into one CbCr plane.
fn interleave_chroma_16(
    destination: *mut c_void,
    stride: usize,
    cb: &Plane,
    cr: &Plane,
    samples: usize,
    height: usize,
) -> Result<()> {
    let row_bytes = samples * 2;
    if samples * 4 > stride {
        return Err(Error::backend(format!(
            "the chroma row is {stride} bytes but {samples} CbCr pairs need {}",
            samples * 4
        )));
    }
    for row in 0..height {
        let cb_row = cb.row(row, row_bytes)?;
        let cr_row = cr.row(row, row_bytes)?;
        // SAFETY: the plane is locked and holds at least `samples * 4` bytes on
        // this row; the loop writes exactly that many and stops.
        unsafe {
            let mut out = destination.cast::<u8>().add(row * stride);
            for sample in 0..samples {
                let offset = sample * 2;
                std::ptr::copy_nonoverlapping(cb_row.as_ptr().add(offset), out, 2);
                out = out.add(2);
                std::ptr::copy_nonoverlapping(cr_row.as_ptr().add(offset), out, 2);
                out = out.add(2);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Reading the buffers VideoToolbox hands back
// ---------------------------------------------------------------------------

/// Copies a decoded `CVPixelBuffer` into AnyCodec planes.
///
/// Returns the layout the buffer *actually* had, its size, and one [`Plane`] per
/// plane of that layout. The buffer's own row strides are preserved, so nothing
/// is silently repacked.
pub fn read_buffer(buffer: &CVPixelBuffer) -> Result<(PixelFormat, u32, u32, Vec<Plane>)> {
    let kind = CVPixelBufferGetPixelFormatType(buffer);
    let Some(format) = pixel_format_from_type(kind) else {
        return Err(Error::unsupported(format!(
            "VideoToolbox produced a CVPixelBuffer of type '{}', which AnyCodec has no matching \
             PixelFormat for: the full-range and 4:2:2 variants are not reinterpreted silently",
            four_char_code(kind as i32)
        )));
    };
    let width = CVPixelBufferGetWidth(buffer);
    let height = CVPixelBufferGetHeight(buffer);
    if width == 0 || height == 0 {
        return Err(Error::backend("the decoded pixel buffer has a zero dimension"));
    }
    // SAFETY: locking a live pixel buffer for reading is the documented
    // precondition for the plane base addresses read below.
    let lock = unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
    if lock != kCVReturnSuccess {
        return Err(Error::backend(format!(
            "CVPixelBufferLockBaseAddress failed: {}",
            os_status_text(lock)
        )));
    }
    let planes = read_planes(buffer, format, width, height);
    // SAFETY: the buffer was locked above with the same flags.
    let unlock = unsafe { CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
    let planes = planes?;
    if unlock != kCVReturnSuccess {
        return Err(Error::backend(format!(
            "CVPixelBufferUnlockBaseAddress failed: {}",
            os_status_text(unlock)
        )));
    }
    Ok((format, width as u32, height as u32, planes))
}

fn read_planes(
    buffer: &CVPixelBuffer,
    format: PixelFormat,
    width: usize,
    height: usize,
) -> Result<Vec<Plane>> {
    let plane_count = CVPixelBufferGetPlaneCount(buffer);
    if plane_count == 0 {
        let stride = CVPixelBufferGetBytesPerRow(buffer);
        let base = CVPixelBufferGetBaseAddress(buffer);
        if base.is_null() || stride < width {
            return Err(Error::backend("the packed pixel buffer has no accessible memory"));
        }
        // SAFETY: the buffer is locked, so `stride * height` bytes are readable.
        let bytes = unsafe { std::slice::from_raw_parts(base.cast::<u8>(), stride * height) };
        return Ok(vec![Plane::new(Buffer::from_slice(bytes), stride)?]);
    }

    if format == PixelFormat::P010 {
        return read_planar_10(buffer, width, height);
    }

    let mut planes = Vec::with_capacity(format.plane_count());
    for index in 0..plane_count.min(format.plane_count()) {
        let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, index);
        let plane_height = CVPixelBufferGetHeightOfPlane(buffer, index);
        let base = CVPixelBufferGetBaseAddressOfPlane(buffer, index);
        if base.is_null() || stride == 0 {
            return Err(Error::backend(format!(
                "plane {index} of the decoded buffer is not accessible"
            )));
        }
        // SAFETY: the buffer is locked, so `stride * plane_height` bytes are readable.
        let bytes =
            unsafe { std::slice::from_raw_parts(base.cast::<u8>(), stride * plane_height) };
        planes.push(Plane::new(Buffer::from_slice(bytes), stride)?);
    }
    Ok(planes)
}

/// Reads CoreVideo's two-plane 10-bit 4:2:0 layout into AnyCodec's three planes.
fn read_planar_10(buffer: &CVPixelBuffer, width: usize, height: usize) -> Result<Vec<Plane>> {
    let luma_stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0);
    let luma = CVPixelBufferGetBaseAddressOfPlane(buffer, 0);
    if luma.is_null() || luma_stride < width * 2 {
        return Err(Error::backend("the 10-bit luma plane is not accessible"));
    }
    // SAFETY: the buffer is locked, so `luma_stride * height` bytes are readable.
    let bytes = unsafe { std::slice::from_raw_parts(luma.cast::<u8>(), luma_stride * height) };
    let luma = Plane::new(Buffer::from_slice(bytes), luma_stride)?;

    let chroma_stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1);
    let chroma_height = CVPixelBufferGetHeightOfPlane(buffer, 1);
    let chroma = CVPixelBufferGetBaseAddressOfPlane(buffer, 1);
    let samples = width.div_ceil(2);
    if chroma.is_null() || chroma_stride < samples * 4 {
        return Err(Error::backend("the 10-bit chroma plane is not accessible"));
    }
    // De-interleave the CbCr pairs into the two planes AnyCodec models.
    let chroma_row_bytes = samples * 2;
    let mut cb = vec![0u8; chroma_row_bytes * chroma_height];
    let mut cr = vec![0u8; chroma_row_bytes * chroma_height];
    // SAFETY: `chroma_stride * chroma_height` bytes of the locked plane are
    // readable and the loops below stay inside that range.
    unsafe {
        let source = chroma.cast::<u8>();
        for row in 0..chroma_height {
            let mut cursor = source.add(row * chroma_stride);
            let mut cb_row = cb.as_mut_ptr().add(row * chroma_row_bytes);
            let mut cr_row = cr.as_mut_ptr().add(row * chroma_row_bytes);
            for _ in 0..samples {
                std::ptr::copy_nonoverlapping(cursor, cb_row, 2);
                cursor = cursor.add(2);
                cb_row = cb_row.add(2);
                std::ptr::copy_nonoverlapping(cursor, cr_row, 2);
                cursor = cursor.add(2);
                cr_row = cr_row.add(2);
            }
        }
    }
    Ok(vec![
        luma,
        Plane::new(Buffer::from_vec(cb), chroma_row_bytes)?,
        Plane::new(Buffer::from_vec(cr), chroma_row_bytes)?,
    ])
}

// ---------------------------------------------------------------------------
// Format descriptions
// ---------------------------------------------------------------------------

/// Builds the raw-video format description that describes our pixel buffers.
///
/// A compressed format description must agree with the image buffer it is used
/// with, and for raw video the "codec type" *is* the pixel format type; CoreMedia
/// spells that out in `CMVideoFormatDescriptionCreate`.
pub fn raw_video_format_description(
    format: PixelFormat,
    width: u32,
    height: u32,
) -> Result<CFRetained<CMVideoFormatDescription>> {
    let Some(codec_type) = pixel_format_type(format) else {
        return Err(Error::unsupported(format!(
            "CoreVideo has no pixel format for {}",
            format.name()
        )));
    };
    bare_format_description(codec_type, width, height)
}

/// Builds a `CMVideoFormatDescription` for `codec_type` at a nominal size.
///
/// For a raw pixel format the codec type *is* the pixel format type; for a
/// compressed codec it is the codec's own identifier. The decoder probe uses this
/// to ask VideoToolbox "can you instantiate a decoder for this codec type at
/// all?", which is a different question from "is this particular stream valid" and
/// is the one a capability report has to answer.
pub fn bare_format_description(
    codec_type: u32,
    width: u32,
    height: u32,
) -> Result<CFRetained<CMVideoFormatDescription>> {
    let mut description: *const CMVideoFormatDescription = std::ptr::null();
    let Some(out) = NonNull::new(&mut description) else {
        return Err(Error::internal("the format description out-parameter cannot be null"));
    };
    // SAFETY: `CMVideoFormatDescriptionCreate` writes a +1 reference into `out` on
    // success; the extensions dictionary is optional.
    let status = unsafe {
        CMVideoFormatDescriptionCreate(
            None,
            codec_type,
            width.max(2) as i32,
            height.max(2) as i32,
            None,
            out,
        )
    };
    if status != 0 {
        return Err(Error::backend(format!(
            "CMVideoFormatDescriptionCreate('{}') failed: {}",
            four_char_code(codec_type as i32),
            os_status_text(status)
        )));
    }
    let Some(pointer) = NonNull::new(description.cast_mut()) else {
        return Err(Error::backend(
            "CMVideoFormatDescriptionCreate reported success but returned no description",
        ));
    };
    // SAFETY: follows the Create rule, so the reference carries +1.
    Ok(unsafe { CFRetained::from_raw(pointer) })
}

/// Builds a compressed-video format description from out-of-band parameter sets.
///
/// The parameter sets come from `extradata` (an `avcC` / `hvcC` record, or bytes
/// that are already Annex-B) and are stripped of their start codes, which is
/// exactly the shape `CMVideoFormatDescriptionCreateFromH264ParameterSets` and its
/// HEVC twin want: raw NAL units plus their lengths.
///
/// Fails with [`Error::invalid_argument`] for a codec that has no parameter-set
/// route at all (VP9, AV1 and VVC carry their configuration in band) and with
/// [`Error::unavailable`] when the record yields no usable parameter sets.
pub fn compressed_video_format_description(
    codec: Codec,
    extradata: &[u8],
    already_annex_b: bool,
) -> Result<CFRetained<CMVideoFormatDescription>> {
    let h264 = codec == Codec::Video(VideoCodec::H264);
    if !h264 && codec != Codec::Video(VideoCodec::H265) {
        return Err(Error::invalid_argument(format!(
            "{codec} has no out-of-band parameter sets; this decoder takes its configuration \
             from the stream"
        )));
    }
    let units = bitstream::annex_b_parameter_sets(codec, extradata, already_annex_b);
    let payloads: Vec<&[u8]> =
        units.iter().map(|unit| strip_start_code(unit)).filter(|unit| !unit.is_empty()).collect();
    if payloads.is_empty() {
        return Err(Error::unavailable(format!(
            "the {codec} decoder needs at least one sequence and one picture parameter set out of \
             band, and the supplied extradata yielded none"
        )));
    }

    let mut pointers: Vec<NonNull<u8>> = Vec::with_capacity(payloads.len());
    let mut sizes: Vec<usize> = Vec::with_capacity(payloads.len());
    for payload in &payloads {
        let Some(pointer) = NonNull::new(payload.as_ptr().cast_mut()) else {
            return Err(Error::internal("a filtered parameter set cannot be empty"));
        };
        pointers.push(pointer);
        sizes.push(payload.len());
    }
    let (Some(pointers), Some(sizes)) =
        (NonNull::new(pointers.as_mut_ptr()), NonNull::new(sizes.as_mut_ptr()))
    else {
        return Err(Error::internal("a pointer array for a non-empty set cannot be null"));
    };

    let mut description: *const CMFormatDescription = std::ptr::null();
    let Some(out) = NonNull::new(&mut description) else {
        return Err(Error::internal("the format description out-parameter cannot be null"));
    };
    // Four-byte lengths, which is what this crate's bitstream conversion emits.
    const NAL_UNIT_HEADER_LENGTH: c_int = 4;
    // SAFETY: every pointer in `pointers` addresses a live `payloads` element and
    // `sizes` has the same length; `payloads` outlives the call.
    let status = unsafe {
        if h264 {
            CMVideoFormatDescriptionCreateFromH264ParameterSets(
                None,
                payloads.len(),
                pointers,
                sizes,
                NAL_UNIT_HEADER_LENGTH,
                out,
            )
        } else {
            CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                None,
                payloads.len(),
                pointers,
                sizes,
                NAL_UNIT_HEADER_LENGTH,
                None,
                out,
            )
        }
    };
    if status != 0 {
        return Err(Error::backend(format!(
            "CMVideoFormatDescriptionCreateFrom{}ParameterSets failed: {}",
            if h264 { "H264" } else { "HEVC" },
            os_status_text(status)
        )));
    }
    let Some(pointer) = NonNull::new(description.cast_mut()) else {
        return Err(Error::backend("CoreMedia reported success but returned no format description"));
    };
    // SAFETY: follows the Create rule, so the reference carries +1.
    Ok(unsafe { CFRetained::from_raw(pointer) })
}

/// Removes a leading Annex-B start code, if the unit carries one.
///
/// `bitstream::annex_b_parameter_sets` returns units *with* a four byte start code
/// when it parsed an `avcC` / `hvcC` record, and *without* one when the input was
/// already Annex-B, so both shapes have to be handled here.
#[must_use]
pub fn strip_start_code(unit: &[u8]) -> &[u8] {
    if let Some(rest) = unit.strip_prefix(&[0u8, 0, 0, 1]) {
        rest
    } else if let Some(rest) = unit.strip_prefix(&[0u8, 0, 1]) {
        rest
    } else {
        unit
    }
}

// ---------------------------------------------------------------------------
// Sample buffers
// ---------------------------------------------------------------------------

/// Wraps a pixel buffer in a `CMSampleBuffer` for a compression session.
///
/// VideoToolbox requires a numeric presentation time on every frame, so the
/// caller's `pts` is interpreted as milliseconds (see the module documentation of
/// [`super`]) and an invalid time never reaches the session.
pub fn image_sample_buffer(
    buffer: &CVPixelBuffer,
    description: &CMVideoFormatDescription,
    pts_ms: i64,
    duration_ms: i64,
) -> Result<CFRetained<CMSampleBuffer>> {
    let timing = CMSampleTimingInfo {
        duration: time_from_ms(duration_ms.max(0)),
        presentationTimeStamp: time_from_ms(pts_ms),
        // SAFETY: reading a `pub static` that CoreMedia initialises.
        decodeTimeStamp: unsafe { objc2_core_media::kCMTimeInvalid },
    };
    let Some(timing) = NonNull::new(&timing as *const CMSampleTimingInfo as *mut CMSampleTimingInfo)
    else {
        return Err(Error::internal("a stack timing struct cannot be null"));
    };
    let image_buffer: &CVImageBuffer = buffer;
    let mut sample: *mut CMSampleBuffer = std::ptr::null_mut();
    let Some(out) = NonNull::new(&mut sample) else {
        return Err(Error::internal("the sample buffer out-parameter cannot be null"));
    };
    // SAFETY: `image_buffer` is a live pixel buffer, `description` describes it,
    // and `timing` outlives the call.
    let status = unsafe {
        CMSampleBuffer::create_for_image_buffer(
            None,
            image_buffer,
            true,
            None,
            std::ptr::null_mut(),
            description,
            timing,
            out,
        )
    };
    if status != 0 {
        return Err(Error::backend(format!(
            "CMSampleBufferCreateForImageBuffer failed: {}",
            os_status_text(status)
        )));
    }
    let Some(pointer) = NonNull::new(sample) else {
        return Err(Error::backend("CoreMedia reported success but returned no sample buffer"));
    };
    // SAFETY: follows the Create rule, so the reference carries +1.
    Ok(unsafe { CFRetained::from_raw(pointer) })
}

/// The image buffer a sample buffer wraps.
///
/// Fails with a specific message when the sample buffer carries no image buffer,
/// which is the case for a compressed or empty one.
pub fn image_of(sample: &CMSampleBuffer) -> Result<CFRetained<CVImageBuffer>> {
    // SAFETY: read-only accessor on a live sample buffer; it returns a retained
    // image buffer or `None`.
    unsafe { sample.image_buffer() }
        .ok_or_else(|| Error::backend("the sample buffer wraps no image buffer"))
}

/// Wraps compressed bytes in a `CMSampleBuffer` for a decompression session.
///
/// The bytes are copied into a `CMBlockBuffer` that CoreMedia owns, so the
/// caller's packet can be dropped the moment this returns.
pub fn bytes_sample_buffer(
    bytes: &[u8],
    description: &CMFormatDescription,
    pts_ms: i64,
    duration_ms: i64,
) -> Result<CFRetained<CMSampleBuffer>> {
    if bytes.is_empty() {
        return Err(Error::invalid_argument("a compressed sample cannot be empty"));
    }
    let block = block_buffer_from_bytes(bytes)?;
    let timing = CMSampleTimingInfo {
        duration: time_from_ms(duration_ms.max(0)),
        presentationTimeStamp: time_from_ms(pts_ms),
        // SAFETY: reading a `pub static` that CoreMedia initialises.
        decodeTimeStamp: unsafe { objc2_core_media::kCMTimeInvalid },
    };
    let Some(timing) = NonNull::new(&timing as *const CMSampleTimingInfo as *mut CMSampleTimingInfo)
    else {
        return Err(Error::internal("a stack timing struct cannot be null"));
    };
    let sizes = [bytes.len()];
    let mut sample: *mut CMSampleBuffer = std::ptr::null_mut();
    let Some(out) = NonNull::new(&mut sample) else {
        return Err(Error::internal("the sample buffer out-parameter cannot be null"));
    };
    // SAFETY: `block` is a live one-sample block buffer, `description` is a live
    // format description, and the two stack locals outlive the call.
    let status = unsafe {
        CMSampleBuffer::create_ready(
            None,
            Some(&block),
            Some(description),
            1,
            1,
            timing.as_ptr(),
            1,
            sizes.as_ptr(),
            out,
        )
    };
    if status != 0 {
        return Err(Error::backend(format!(
            "CMSampleBufferCreateReady failed: {}",
            os_status_text(status)
        )));
    }
    let Some(pointer) = NonNull::new(sample) else {
        return Err(Error::backend("CoreMedia reported success but returned no sample buffer"));
    };
    // SAFETY: follows the Create rule, so the reference carries +1.
    Ok(unsafe { CFRetained::from_raw(pointer) })
}

/// Allocates a `CMBlockBuffer` and copies `bytes` into it.
fn block_buffer_from_bytes(bytes: &[u8]) -> Result<CFRetained<CMBlockBuffer>> {
    // SAFETY: the default allocator is a framework constant.
    let allocator = unsafe { objc2_core_foundation::kCFAllocatorDefault };
    let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
    let Some(out) = NonNull::new(&mut block) else {
        return Err(Error::internal("the block buffer out-parameter cannot be null"));
    };
    // A NULL `memory_block` with the default block allocator asks CoreMedia to
    // allocate the storage, so nothing has to outlive this call.
    // SAFETY: every pointer argument is either NULL, which the API documents as
    // "allocate for me", or a valid local.
    let status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,
            std::ptr::null_mut(),
            bytes.len(),
            allocator,
            std::ptr::null(),
            0,
            bytes.len(),
            kCMBlockBufferAssureMemoryNowFlag,
            out,
        )
    };
    if status != 0 {
        return Err(Error::backend(format!(
            "CMBlockBufferCreateWithMemoryBlock failed: {}",
            os_status_text(status)
        )));
    }
    let Some(pointer) = NonNull::new(block) else {
        return Err(Error::backend("CoreMedia reported success but returned no block buffer"));
    };
    // SAFETY: follows the Create rule, so the reference carries +1.
    let block = unsafe { CFRetained::from_raw(pointer) };
    let Some(source) = NonNull::new(bytes.as_ptr().cast_mut().cast::<c_void>()) else {
        return Err(Error::internal("a non-empty byte slice cannot have a null pointer"));
    };
    // SAFETY: `block` owns at least `bytes.len()` bytes and `source` addresses
    // exactly that many readable bytes.
    let status = unsafe { CMBlockBuffer::replace_data_bytes(source, &block, 0, bytes.len()) };
    if status != 0 {
        return Err(Error::backend(format!(
            "CMBlockBufferReplaceDataBytes failed: {}",
            os_status_text(status)
        )));
    }
    Ok(block)
}

/// Reads one compressed picture back out of a `CMSampleBuffer`.
///
/// Returns the Annex-B payload, its timestamp in milliseconds, and whether it is
/// a keyframe. Any length prefixes become four byte start codes, so the packet
/// matches what every other backend in this crate emits.
///
/// `Ok(None)` means "this sample buffer carried no picture" (an empty sample, or
/// one the decoder dropped), never an error.
pub fn bytes_from_sample(sample: &CMSampleBuffer) -> Result<Option<(Vec<u8>, i64, bool)>> {
    // SAFETY: read-only accessors on a live sample buffer.
    let num_samples = unsafe { sample.num_samples() };
    if num_samples <= 0 {
        return Ok(None);
    }
    // SAFETY: as above; `data_buffer` returns a retained block buffer or `None`.
    let Some(block) = (unsafe { sample.data_buffer() }) else {
        return Ok(None);
    };
    // SAFETY: as above.
    let total = unsafe { block.data_length() };
    if total == 0 {
        return Ok(None);
    }
    let mut pointer: *mut c_char = std::ptr::null_mut();
    let mut length_at_offset = 0usize;
    // SAFETY: `data_pointer` writes the start and length of a contiguous run into
    // the two out-parameters, both of which are valid locals.
    let status =
        unsafe { block.data_pointer(0, &mut length_at_offset, std::ptr::null_mut(), &mut pointer) };
    if status != 0 {
        return Err(Error::backend(format!(
            "CMBlockBufferGetDataPointer failed: {}",
            os_status_text(status)
        )));
    }
    if pointer.is_null() || length_at_offset == 0 {
        return Err(Error::backend("the compressed sample buffer has no readable bytes"));
    }
    // SAFETY: the sample's block buffer reported `length_at_offset` contiguous
    // bytes starting at `pointer`.
    let payload =
        unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length_at_offset.min(total)) };
    let data = length_prefixed_to_annex_b(payload);
    // SAFETY: read-only accessors on a live sample buffer. The *output* timestamp
    // is the one VideoToolbox stamps on the decoded frame.
    let time = unsafe { sample.output_presentation_time_stamp() };
    Ok(Some((data, ms_from_time(time), sample_is_sync(sample))))
}

/// `true` when the sample buffer does not carry `kCMSampleAttachmentKey_NotSync`.
///
/// VideoToolbox only writes that attachment when a sample is *not* a sync sample,
/// so an absent attachment means "this is a keyframe".
fn sample_is_sync(sample: &CMSampleBuffer) -> bool {
    // SAFETY: read-only accessor; asking it not to create the array keeps this a
    // pure read. The array it returns holds one mutable dictionary per sample,
    // whose values are the documented attachment `CFType`s.
    let Some(attachments) = (unsafe { sample.sample_attachments_array(false) }) else {
        return true;
    };
    // SAFETY: a sample attachment array is an array of dictionaries keyed by
    // `CFString` and holding `CFType` values.
    let attachments: CFRetained<
        objc2_core_foundation::CFArray<
            objc2_core_foundation::CFDictionary<objc2_core_foundation::CFString, CFType>,
        >,
    > = unsafe { CFRetained::cast_unchecked(attachments) };
    let Some(entry) = attachments.get(0) else {
        return true;
    };
    // SAFETY: reading a framework constant that CoreMedia initialises.
    let not_sync_key = unsafe { objc2_core_media::kCMSampleAttachmentKey_NotSync };
    let Some(value) = entry.get(not_sync_key) else {
        return true;
    };
    // The key holds a CFBoolean; anything else that is present and non-zero also
    // means "not a sync sample".
    if let Some(boolean) = value.downcast_ref::<objc2_core_foundation::CFBoolean>() {
        return !boolean.as_bool();
    }
    if let Some(number) = value.downcast_ref::<objc2_core_foundation::CFNumber>() {
        return number.as_i32().unwrap_or(0) == 0;
    }
    false
}

/// Rewrites the four byte big-endian length prefixes of `payload` as Annex-B
/// start codes.
///
/// Input that is not a whole run of length-prefixed units is returned unchanged,
/// so a decoder that wants in-band start codes still gets the bytes it was handed.
pub fn length_prefixed_to_annex_b(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 16);
    let mut cursor = 0usize;
    while cursor + 4 <= payload.len() {
        let length = u32::from_be_bytes([
            payload[cursor],
            payload[cursor + 1],
            payload[cursor + 2],
            payload[cursor + 3],
        ]) as usize;
        cursor += 4;
        let end = cursor + length;
        if length == 0 || end > payload.len() {
            // Not a length-prefixed unit after all.
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&payload[cursor..end]);
        cursor = end;
    }
    if out.is_empty() || cursor != payload.len() {
        // Either nothing was converted, or there is a tail that is not a complete
        // unit: hand back the original bytes rather than a mangled stream.
        return payload.to_vec();
    }
    out
}

/// Converts a caller's packet into the Annex-B payload a decoder wants.
///
/// Packets that already use start codes are passed through; length-prefixed ones
/// are converted. `HEADER` and `EOS` packets never reach here 鈥?the sessions
/// handle those themselves.
pub fn packet_payload(packet: &Packet) -> Result<Vec<u8>> {
    let bytes = packet.data.as_slice();
    if bytes.is_empty() {
        return Err(Error::invalid_argument("the packet carries no compressed bytes"));
    }
    if bytes.starts_with(&[0, 0, 1]) || bytes.starts_with(&[0, 0, 0, 1]) {
        Ok(bytes.to_vec())
    } else {
        Ok(length_prefixed_to_annex_b(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pixel_format_table_is_total_and_round_trips() {
        for format in PixelFormat::ALL {
            match pixel_format_type(format) {
                Some(kind) => assert_eq!(
                    pixel_format_from_type(kind),
                    Some(format),
                    "{format:?} must round-trip through its kCVPixelFormatType"
                ),
                None => assert_eq!(
                    format,
                    PixelFormat::Nv21,
                    "{format:?} must have a CoreVideo pixel format: only V,U-order 4:2:0 has none"
                ),
            }
        }
        // And the inverse direction is total over exactly the mapped values.
        assert_eq!(
            pixel_format_from_type(kCVPixelFormatType_420YpCbCr8Planar),
            Some(PixelFormat::I420)
        );
        assert_eq!(
            pixel_format_from_type(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange),
            Some(PixelFormat::Nv12)
        );
        assert_eq!(pixel_format_from_type(kCVPixelFormatType_32RGBA), Some(PixelFormat::Rgba8));
        assert_eq!(pixel_format_from_type(kCVPixelFormatType_32BGRA), Some(PixelFormat::Bgra8));
        assert_eq!(
            pixel_format_from_type(kCVPixelFormatType_422YpCbCr8_yuvs),
            Some(PixelFormat::Yuy2)
        );
        assert_eq!(
            pixel_format_from_type(kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange),
            Some(PixelFormat::P010)
        );
    }

    #[test]
    fn only_nv21_is_unsupported() {
        for format in PixelFormat::ALL {
            assert_eq!(is_supported(format), format != PixelFormat::Nv21, "{format:?}");
        }
    }

    #[test]
    fn the_full_range_and_other_variants_are_not_reinterpreted() {
        // '420f' has the same plane order as '420v' but a different colour range,
        // and '2vuy' keeps its chroma in the opposite order to AnyCodec's YUY2.
        for kind in [
            objc2_core_video::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            objc2_core_video::kCVPixelFormatType_420YpCbCr8PlanarFullRange,
            objc2_core_video::kCVPixelFormatType_422YpCbCr8,
            objc2_core_video::kCVPixelFormatType_422YpCbCr8FullRange,
            objc2_core_video::kCVPixelFormatType_420YpCbCr10BiPlanarFullRange,
            objc2_core_video::kCVPixelFormatType_32ARGB,
        ] {
            assert_eq!(pixel_format_from_type(kind), None, "'{}'", four_char_code(kind as i32));
        }
    }

    #[test]
    fn the_pixel_format_values_are_the_documented_four_character_codes() {
        assert_eq!(pixel_format_type(PixelFormat::I420), Some(0x7934_3230)); // 'y420'
        assert_eq!(pixel_format_type(PixelFormat::Nv12), Some(0x3432_3076)); // '420v'
        assert_eq!(pixel_format_type(PixelFormat::Bgra8), Some(0x4247_5241)); // 'BGRA'
        assert_eq!(pixel_format_type(PixelFormat::Rgba8), Some(0x5247_4241)); // 'RGBA'
        assert_eq!(pixel_format_type(PixelFormat::Yuy2), Some(0x7975_7673)); // 'yuvs'
        assert_eq!(pixel_format_type(PixelFormat::P010), Some(0x7834_3230)); // 'x420'
    }

    #[test]
    fn start_codes_are_stripped_from_both_shapes() {
        assert_eq!(strip_start_code(&[0, 0, 0, 1, 0x67, 0x11]), &[0x67, 0x11]);
        assert_eq!(strip_start_code(&[0, 0, 1, 0x67, 0x11]), &[0x67, 0x11]);
        assert_eq!(strip_start_code(&[0x67, 0x11]), &[0x67, 0x11]);
        assert_eq!(strip_start_code(&[]), &[] as &[u8]);
    }

    #[test]
    fn length_prefixed_units_become_annex_b() {
        let payload = [0u8, 0, 0, 2, 0x65, 0x11, 0, 0, 0, 3, 0x41, 0x22, 0x33];
        let annex_b = length_prefixed_to_annex_b(&payload);
        assert_eq!(annex_b, vec![0, 0, 0, 1, 0x65, 0x11, 0, 0, 0, 1, 0x41, 0x22, 0x33]);
    }

    #[test]
    fn a_truncated_or_unprefixed_payload_is_passed_through_untouched() {
        let raw = [0x65u8, 0x11, 0x22, 0x33, 0x44];
        assert_eq!(length_prefixed_to_annex_b(&raw), raw.to_vec());
        let truncated = [0u8, 0, 0, 9, 0x65];
        assert_eq!(length_prefixed_to_annex_b(&truncated), truncated.to_vec());
        // A valid unit followed by an incomplete tail falls back to the input.
        let mixed = [0u8, 0, 0, 1, 0x65, 0, 0, 0, 9, 0x41];
        assert_eq!(length_prefixed_to_annex_b(&mixed), mixed.to_vec());
        assert!(length_prefixed_to_annex_b(&[]).is_empty());
    }

    #[test]
    fn an_annex_b_payload_is_not_double_converted() {
        let annex = [0u8, 0, 0, 1, 0x65, 0x11];
        assert_eq!(length_prefixed_to_annex_b(&annex), annex.to_vec());
    }

    #[test]
    fn packets_keep_their_annex_b_and_gain_it_when_they_lack_it() {
        let codec = Codec::Video(VideoCodec::H264);
        let annex = Packet::new(codec, Buffer::from_slice(&[0, 0, 0, 1, 0x65, 0x11]), 0, 0);
        assert_eq!(packet_payload(&annex).unwrap(), vec![0, 0, 0, 1, 0x65, 0x11]);
        let three = Packet::new(codec, Buffer::from_slice(&[0, 0, 1, 0x65, 0x11]), 0, 0);
        assert_eq!(packet_payload(&three).unwrap(), vec![0, 0, 1, 0x65, 0x11]);
        let prefixed = Packet::new(codec, Buffer::from_slice(&[0, 0, 0, 2, 0x65, 0x11]), 0, 0);
        assert_eq!(packet_payload(&prefixed).unwrap(), vec![0, 0, 0, 1, 0x65, 0x11]);
        let empty = Packet::new(codec, Buffer::new(), 0, 0);
        assert!(packet_payload(&empty).is_err());
    }

    #[test]
    fn a_zero_sized_or_unmappable_buffer_is_rejected_before_corevideo_sees_it() {
        assert!(create_buffer(PixelFormat::Nv12, 0, 16).is_err());
        assert!(create_buffer(PixelFormat::Nv21, 16, 16).is_err());
    }

    #[test]
    #[ignore = "requires a macOS media engine"]
    fn corevideo_allocates_a_pixel_buffer_for_every_mapped_format() {
        for format in PixelFormat::ALL {
            let buffer = create_buffer(format, 16, 16);
            assert_eq!(
                buffer.is_ok(),
                pixel_format_type(format).is_some(),
                "{format:?}: {:?}",
                buffer.err().map(|error| error.message().to_string())
            );
        }
    }

    #[test]
    fn a_format_description_can_be_built_for_every_mapped_pixel_format() {
        for format in PixelFormat::ALL {
            let description = raw_video_format_description(format, 16, 16);
            assert_eq!(description.is_ok(), pixel_format_type(format).is_some(), "{format:?}");
        }
    }

    #[test]
    fn a_codec_without_parameter_sets_says_so_specifically() {
        // No framework call at all: this fails before CoreMedia is reached.
        let error = compressed_video_format_description(
            Codec::Video(VideoCodec::Vp9),
            &[0, 0, 0, 1, 0x67],
            true,
        )
        .expect_err("vp9 has no out-of-band parameter sets");
        assert!(error.message().contains("vp9"), "{}", error.message());
        let error = compressed_video_format_description(Codec::Video(VideoCodec::H264), &[], false)
            .expect_err("empty extradata yields no parameter sets");
        assert!(error.message().contains("parameter set"), "{}", error.message());
    }

    #[test]
    #[ignore = "requires a macOS media engine"]
    fn the_h264_format_description_comes_from_the_parameter_sets() {
        const AVCC: [u8; 16] = [
            0x01, 0x64, 0x00, 0x1f, 0xff, 0xe1, 0x00, 0x03, 0x67, 0x64, 0x00, 0x01, 0x00, 0x02,
            0x68, 0xee,
        ];
        let description = compressed_video_format_description(Codec::Video(VideoCodec::H264), &AVCC, false);
        assert!(
            description.is_ok(),
            "{:?}",
            description.err().map(|error| error.message().to_string())
        );
    }
}
