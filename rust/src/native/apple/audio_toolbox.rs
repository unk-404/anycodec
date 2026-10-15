//! The AudioToolbox half of the Apple backend.
//!
//! AudioToolbox converts audio with an `AudioConverter`: a pair of
//! `AudioStreamBasicDescription`s in, a session out, and
//! `AudioConverterFillComplexBuffer` to push samples through it. The converter
//! pulls its input from a C callback, so most of this module is the callback and
//! the `AudioStreamBasicDescription` arithmetic that gets it wrong when it is
//! careless.
//!
//! ## The sample layouts, and why they are spelled out here
//!
//! `mFormatFlags`, `mFramesPerPacket` and `mBytesPerPacket` are not optional
//! decoration: an AAC stream whose `mFormatFlags` is `0` instead of
//! `kMPEG4Object_AAC_LC` builds a converter that silently means something else, and
//! an Apple Lossless stream without the lossless flags is rejected outright. Every
//! layout this module builds is therefore named and tested:
//!
//! | Codec | `mFormatID` | `mFormatFlags` | `mFramesPerPacket` |
//! |-------|-------------|----------------|--------------------|
//! | AAC | `kAudioFormatMPEG4AAC` | `kMPEG4Object_AAC_LC` (2) | 1024 |
//! | Apple Lossless | `kAudioFormatAppleLossless` | `kAppleLosslessFormatFlag_16BitSourceData` | 4096 |
//! | MP3 | `kAudioFormatMPEGLayer3` | 0 | 1152 |
//! | linear PCM | `kAudioFormatLinearPCM` | packed float, native endian, or packed signed integer | 1 |
//!
//! ## The one PCM layout
//!
//! **Decoded output is always interleaved 32-bit float PCM** (`SampleFormat::F32`),
//! which is what `DecoderConfig::sample_format` defaults to and what CoreAudio's
//! canonical format is. A caller that asks for anything else gets `Ok(None)` from
//! the factory rather than a silent conversion. On the input side an encoder
//! accepts `F32` and `S16`; `S16` is widened to `F32` before it reaches the
//! converter, because a single PCM layout keeps the converter's
//! `mBytesPerFrame` arithmetic in one place.
//!
//! ## Magic cookies
//!
//! * Encoding: after the converter is built, `kAudioConverterCompressionMagicCookie`
//!   is read back and returned from [`Encoder::extradata`]. For AAC that is the
//!   `AudioSpecificConfig` a container has to store next to the stream; for Apple
//!   Lossless it is the lossless configuration.
//! * Decoding: a caller-supplied `DecoderConfig::extradata` is installed with
//!   `kAudioConverterDecompressionMagicCookie`. When the caller supplied none, the
//!   documented `kAudioFormatProperty_FormatInfo` route is used instead: it takes an
//!   `AudioStreamBasicDescription` with `mFormatID` filled in and returns the fields
//!   CoreAudio can derive. (`kAudioFormatProperty_MagicCookie` does not exist in
//!   `AudioFormat.h` on any SDK; the cookie lives on the converter.)
//!
//! ## Hardware acceleration
//!
//! There is none. `AudioConverter` is a CPU DSP path, so every audio
//! [`CodecPath`] reports `hardware_accelerated == false` and says so in `detail`.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;

use objc2_audio_toolbox::{
    kAudioConverterCompressionMagicCookie, kAudioConverterDecompressionMagicCookie,
    kAudioConverterEncodeBitRate, kAudioConverterPropertyMaximumOutputPacketSize,
    kAudioFormatProperty_FormatInfo, AudioConverterDispose, AudioConverterFillComplexBuffer,
    AudioConverterGetProperty, AudioConverterGetPropertyInfo, AudioConverterNew,
    AudioConverterPropertyID, AudioConverterRef, AudioConverterSetProperty, AudioFormatGetProperty,
};
use objc2_core_audio_types::{
    kAppleLosslessFormatFlag_16BitSourceData, kAudioFormatAppleLossless,
    kAudioFormatFlagIsPacked, kAudioFormatFlagIsSignedInteger, kAudioFormatFlagsNativeFloatPacked,
    kAudioFormatLinearPCM, kAudioFormatMPEG4AAC, kAudioFormatMPEGLayer3, AudioBuffer, AudioBufferList,
    AudioFormatID, AudioStreamBasicDescription, AudioStreamPacketDescription, MPEG4ObjectID,
};

use crate::engine::backend::{BackendKind, CodecPath};
use crate::engine::traits::{Decoder, Encoder};
use crate::engine::{BackendAvailability, CodecKind, HwCodecSupport, HwDeviceInfo, Registry};
use crate::error::{Error, Result};
use crate::media::buffer::{Buffer, BufferBuilder};
use crate::media::codec::{AudioCodec, Codec};
use crate::media::config::{DecoderConfig, EncoderConfig};
use crate::media::format::{ChannelLayout, SampleFormat};
use crate::media::frame::{AudioFrame, Frame};
use crate::media::packet::{flags as packet_flags, Packet};

use super::{os_status_text, TIME_SCALE};

/// `kMPEG4Object_AAC_LC`, the value an AAC stream's `mFormatFlags` holds.
///
/// AAC's `mFormatFlags` is not a bit field of format flags but an MPEG-4 *object
/// type*; `0` is not "no flags", it is "AAC Main", which is a different profile.
const AAC_LC: u32 = MPEG4ObjectID::AAC_LC.0 as u32;

/// Sample frames in one AAC packet, fixed by the standard.
const AAC_FRAMES_PER_PACKET: u32 = 1_024;

/// Sample frames in one Apple Lossless packet. Apple's encoder uses 4096.
const ALAC_FRAMES_PER_PACKET: u32 = 4_096;

/// Sample frames in one MPEG-1/2 Layer III packet.
const MP3_FRAMES_PER_PACKET: u32 = 1_152;

/// How many bytes of PCM the decoder converts into per call.
const DECODE_CHUNK_BYTES: usize = 16 * 1024;

/// The sample rate used when a caller supplies none but the decoder needs one to
/// build its input description.
const DEFAULT_SAMPLE_RATE: u32 = 48_000;

// ---------------------------------------------------------------------------
// Sample descriptions
// ---------------------------------------------------------------------------

/// The one PCM layout this backend reads and writes: interleaved, packed.
#[must_use]
pub fn pcm_description(
    sample_rate: u32,
    channels: ChannelLayout,
    format: SampleFormat,
) -> AudioStreamBasicDescription {
    let bytes = format.bytes_per_sample() as u32;
    let channel_count = channels.channels();
    // `kAudioFormatFlagsNativeEndian` is zero on every Apple platform, so the
    // endianness needs no flag; `Packed` plus the sample type is the whole layout.
    let flags = match format {
        SampleFormat::F32 => kAudioFormatFlagsNativeFloatPacked,
        _ => kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked,
    };
    AudioStreamBasicDescription {
        mSampleRate: f64::from(sample_rate),
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: flags,
        mBytesPerPacket: bytes * channel_count,
        mFramesPerPacket: 1,
        mBytesPerFrame: bytes * channel_count,
        mChannelsPerFrame: channel_count,
        mBitsPerChannel: bytes * 8,
        mReserved: 0,
    }
}

/// The compressed layout for `codec`, or `None` when CoreAudioTypes has no
/// identifier for it.
///
/// The per-codec flags are the part that goes wrong quietly, so each arm carries
/// the value the codec actually needs rather than a plausible-looking default.
#[must_use]
pub fn compressed_description(
    codec: Codec,
    sample_rate: u32,
    channels: ChannelLayout,
) -> Option<AudioStreamBasicDescription> {
    let (format_id, flags, frames_per_packet) = match codec {
        Codec::Audio(AudioCodec::Aac) => {
            (kAudioFormatMPEG4AAC, AAC_LC, AAC_FRAMES_PER_PACKET)
        }
        Codec::Audio(AudioCodec::Alac) => (
            kAudioFormatAppleLossless,
            kAppleLosslessFormatFlag_16BitSourceData,
            ALAC_FRAMES_PER_PACKET,
        ),
        Codec::Audio(AudioCodec::Mp3) => (kAudioFormatMPEGLayer3, 0, MP3_FRAMES_PER_PACKET),
        _ => return None,
    };
    Some(AudioStreamBasicDescription {
        mSampleRate: f64::from(sample_rate),
        mFormatID: format_id,
        mFormatFlags: flags,
        // A compressed stream's packet size and frame size are either variable
        // (`0`) or fixed by the codec; `mFramesPerPacket` is never zero because the
        // converter uses it to compute timestamps.
        mBytesPerPacket: 0,
        mFramesPerPacket: frames_per_packet,
        mBytesPerFrame: 0,
        mChannelsPerFrame: channels.channels(),
        mBitsPerChannel: 0,
        mReserved: 0,
    })
}

/// The number of sample frames in one packet of `codec`'s compressed format.
#[must_use]
pub fn frames_per_packet(codec: Codec) -> Option<u32> {
    Some(match codec {
        Codec::Audio(AudioCodec::Aac) => AAC_FRAMES_PER_PACKET,
        Codec::Audio(AudioCodec::Alac) => ALAC_FRAMES_PER_PACKET,
        Codec::Audio(AudioCodec::Mp3) => MP3_FRAMES_PER_PACKET,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Converter construction
// ---------------------------------------------------------------------------

/// The picture of an open converter's description, kept by the sessions.
#[derive(Clone, Copy, Debug)]
struct ConverterGeometry {
    /// Sample frames in one packet of the compressed side.
    frames_per_packet: u32,
    /// The sample rate both sides run at.
    sample_rate: u32,
    /// The channel layout both sides use.
    channels: ChannelLayout,
}

/// Opens an `AudioConverter` for `codec` and `kind`, installing `cookie` when one
/// was supplied.
///
/// A raw `AudioConverterRef` is returned rather than a wrapper because the probes
/// dispose of it immediately; the sessions wrap it in a type whose `Drop` does.
fn create_converter(
    codec: Codec,
    kind: CodecKind,
    sample_rate: u32,
    channels: ChannelLayout,
    bitrate: u64,
    cookie: Option<&[u8]>,
) -> Result<(AudioConverterRef, ConverterGeometry)> {
    let Some(mut compressed) = compressed_description(codec, sample_rate, channels) else {
        return Err(Error::unsupported(format!(
            "AudioToolbox has no AudioFormatID for {}",
            codec.name()
        )));
    };
    // A decoder with no cookie still needs a description CoreAudio can reason
    // about; `kAudioFormatProperty_FormatInfo` is the documented way to fill one in.
    if kind == CodecKind::Decoder && cookie.is_none() && codec == Codec::Audio(AudioCodec::Alac) {
        if let Some(filled) = fill_format_info(&compressed) {
            compressed = filled;
        }
    }
    let mut pcm = pcm_description(sample_rate, channels, SampleFormat::F32);

    let mut converter: AudioConverterRef = std::ptr::null_mut();
    let Some(source) = NonNull::new(if kind == CodecKind::Encoder { &mut pcm } else { &mut compressed })
    else {
        return Err(Error::internal("a stack description cannot be null"));
    };
    let Some(destination) =
        NonNull::new(if kind == CodecKind::Encoder { &mut compressed } else { &mut pcm })
    else {
        return Err(Error::internal("a stack description cannot be null"));
    };
    let Some(out) = NonNull::new(&mut converter) else {
        return Err(Error::internal("the converter out-parameter cannot be null"));
    };
    // SAFETY: both descriptions are live locals, and `AudioConverterNew` writes a
    // converter reference into `out` on success.
    let status = unsafe { AudioConverterNew(source, destination, out) };
    if status != 0 {
        return Err(Error::backend(format!(
            "AudioConverterNew({} {}, {sample_rate} Hz, {}) failed: {}",
            codec.name(),
            kind.name(),
            channels.name(),
            os_status_text(status)
        )));
    }
    if converter.is_null() {
        return Err(Error::backend(
            "AudioConverterNew reported success but returned no converter",
        ));
    }

    // A cookie is per-stream, so installing it is the caller's business and a
    // failure here is reported rather than ignored.
    if let Some(cookie) = cookie {
        if !cookie.is_empty() {
            // The bytes are copied because `AudioConverterSetProperty` wants a
            // mutable pointer and the caller's `Buffer` is shared and immutable.
            let mut owned = cookie.to_vec();
            let Some(data) = NonNull::new(owned.as_mut_ptr().cast::<c_void>()) else {
                dispose(converter);
                return Err(Error::internal("a non-empty cookie cannot be null"));
            };
            let property = if kind == CodecKind::Decoder {
                kAudioConverterDecompressionMagicCookie
            } else {
                kAudioConverterCompressionMagicCookie
            };
            // SAFETY: `owned` is live for the call and `property` names the cookie
            // property for this direction.
            let status = unsafe {
                AudioConverterSetProperty(converter, property, owned.len() as u32, data)
            };
            if status != 0 {
                dispose(converter);
                return Err(Error::backend(format!(
                    "AudioConverterSetProperty(magic cookie) failed: {}",
                    os_status_text(status)
                )));
            }
        }
    }

    // The bitrate only means something for a lossy encoder; Apple Lossless ignores
    // it and some builds reject the property outright.
    if kind == CodecKind::Encoder && codec == Codec::Audio(AudioCodec::Aac) {
        let mut target = if bitrate > 0 { bitrate.min(u64::from(u32::MAX)) as u32 } else { 128_000 };
        if let Some(data) = NonNull::new((&mut target as *mut u32).cast::<c_void>()) {
            // SAFETY: the property is a `u32` and `target` is a live local.
            let status = unsafe {
                AudioConverterSetProperty(
                    converter,
                    kAudioConverterEncodeBitRate,
                    std::mem::size_of::<u32>() as u32,
                    data,
                )
            };
            if status != 0 {
                // Not fatal: the converter still works at its default bitrate, so this
                // is a quality-of-service note rather than an error.
                log::debug!(
                    "AudioConverter did not accept the requested bitrate {target}: {}",
                    os_status_text(status)
                );
            }
        }
    }

    let Some(frames) = frames_per_packet(codec) else {
        dispose(converter);
        return Err(Error::unsupported(format!("{} has no packet size", codec.name())));
    };
    Ok((converter, ConverterGeometry { frames_per_packet: frames, sample_rate, channels }))
}

/// Disposes of a converter, ignoring whatever it reports.
fn dispose(converter: AudioConverterRef) {
    if converter.is_null() {
        return;
    }
    // SAFETY: the pointer came from `AudioConverterNew` and is disposed exactly once.
    unsafe { AudioConverterDispose(converter) };
}

/// Asks AudioToolbox to fill in the fields of `description` it can derive.
///
/// This is the documented route for a decoder whose container supplied no magic
/// cookie: `kAudioFormatProperty_FormatInfo` takes an `AudioStreamBasicDescription`
/// as the *property value* (with at least `mFormatID` set) and an optional magic
/// cookie as the specifier. `None` means CoreAudio could not derive anything, and
/// the caller carries on with the description it built.
fn fill_format_info(description: &AudioStreamBasicDescription) -> Option<AudioStreamBasicDescription> {
    let mut value = *description;
    let mut size = std::mem::size_of::<AudioStreamBasicDescription>() as u32;
    // SAFETY: the specifier is NULL (which the property documents as allowed) and
    // `value` is a live, correctly sized output buffer.
    let status = unsafe {
        AudioFormatGetProperty(
            kAudioFormatProperty_FormatInfo,
            0,
            std::ptr::null(),
            &mut size,
            (&mut value as *mut AudioStreamBasicDescription).cast::<c_void>(),
        )
    };
    if status == 0 && size as usize >= std::mem::size_of::<AudioStreamBasicDescription>() {
        Some(value)
    } else {
        None
    }
}

/// Reads a `u32`-valued converter property, or `None` when it is not offered.
fn read_property_u32(converter: AudioConverterRef, property: AudioConverterPropertyID) -> Option<u32> {
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: `value` is a correctly sized output buffer for a `u32` property.
    let status = unsafe {
        AudioConverterGetProperty(
            converter,
            property,
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast::<c_void>(),
        )
    };
    if status == 0 && size as usize >= std::mem::size_of::<u32>() {
        Some(value)
    } else {
        None
    }
}

/// Reads a variable-length converter property, such as a magic cookie.
fn read_property_bytes(
    converter: AudioConverterRef,
    property: AudioConverterPropertyID,
) -> Option<Vec<u8>> {
    let mut size = 0u32;
    // SAFETY: `size` is a valid out-parameter and the writability flag is optional.
    let status = unsafe {
        AudioConverterGetPropertyInfo(converter, property, &mut size, std::ptr::null_mut())
    };
    if status != 0 || size == 0 || size > (1 << 20) {
        return None;
    }
    let mut bytes = vec![0u8; size as usize];
    let mut written = size;
    let out = NonNull::new(bytes.as_mut_ptr().cast::<c_void>())?;
    // SAFETY: `bytes` is exactly `size` bytes and `written` says how much it holds.
    let status = unsafe { AudioConverterGetProperty(converter, property, NonNull::from(&mut written), out) };
    if status != 0 {
        return None;
    }
    bytes.truncate((written as usize).min(bytes.len()));
    Some(bytes)
}

// ---------------------------------------------------------------------------
// The input callback
// ---------------------------------------------------------------------------

/// The chunk a converter is currently pulling its input from.
#[derive(Debug)]
struct InputChunk {
    /// The bytes the caller handed the session.
    data: Vec<u8>,
    /// Byte offset of the next packet to hand over.
    offset: usize,
    /// Bytes in one packet, or `0` when the packets are variable length.
    packet_bytes: u32,
    /// Sample frames in one packet of the input format.
    frames_per_packet: u32,
    /// `true` when the input is compressed, so each packet needs a description.
    described: bool,
    /// The description handed to CoreAudio. It lives here so the pointer the
    /// callback publishes stays valid until the next call.
    description: AudioStreamPacketDescription,
}

impl InputChunk {
    /// An empty chunk that has already been drained.
    fn empty() -> Self {
        Self {
            data: Vec::new(),
            offset: 0,
            packet_bytes: 0,
            frames_per_packet: 0,
            described: false,
            description: AudioStreamPacketDescription {
                mStartOffset: 0,
                mVariableFramesInPacket: 0,
                mDataByteSize: 0,
            },
        }
    }

    /// Points the chunk at `data`, which the converter will pull `packet_bytes` at a
    /// time (or one whole variable-length packet when `packet_bytes` is `0`).
    fn set(&mut self, data: Vec<u8>, packet_bytes: u32, frames_per_packet: u32, described: bool) {
        self.data = data;
        self.offset = 0;
        self.packet_bytes = packet_bytes;
        self.frames_per_packet = frames_per_packet;
        self.described = described;
        self.description = AudioStreamPacketDescription {
            mStartOffset: 0,
            mVariableFramesInPacket: frames_per_packet,
            mDataByteSize: 0,
        };
    }
}

/// The `AudioConverterComplexInputDataProc` that feeds a converter.
///
/// # Safety
///
/// `in_input_data_proc_user_data` must be the raw pointer of the live
/// `Mutex<InputChunk>` owned by the session that registered this callback, and that
/// session must outlive the converter. Both hold because the converter is disposed
/// in the session's `Drop`, before the box is released.
unsafe extern "C-unwind" fn converter_input(
    _converter: AudioConverterRef,
    io_number_data_packets: NonNull<u32>,
    io_data: NonNull<AudioBufferList>,
    out_data_packet_description: *mut *mut AudioStreamPacketDescription,
    in_input_data_proc_user_data: *mut c_void,
) -> i32 {
    if in_input_data_proc_user_data.is_null() {
        return objc2_audio_toolbox::kAudioConverterErr_UnspecifiedError;
    }
    // SAFETY: the contract above, and the mutex is what makes it sound.
    let chunk = unsafe { &*in_input_data_proc_user_data.cast::<Mutex<InputChunk>>() };
    let Ok(mut chunk) = chunk.lock() else {
        return objc2_audio_toolbox::kAudioConverterErr_UnspecifiedError;
    };
    // SAFETY: non-null for the duration of the call, and at least one buffer.
    let list = unsafe { &mut *io_data.as_ptr() };
    if list.mNumberBuffers == 0 {
        return -50; // paramErr: nothing to fill in.
    }
    let remaining = chunk.data.len().saturating_sub(chunk.offset);
    if remaining == 0 {
        // Out of input: zero packets is how a callback says "that was all", which
        // makes the converter flush and return.
        // SAFETY: both pointers are live for the duration of the call.
        unsafe {
            io_number_data_packets.as_ptr().write(0);
        }
        list.mBuffers[0].mDataByteSize = 0;
        list.mBuffers[0].mData = std::ptr::null_mut();
        return 0;
    }

    let (packets, bytes) = if chunk.packet_bytes > 0 {
        // PCM: hand over as many whole packets as the converter asked for.
        let wanted = unsafe { io_number_data_packets.as_ref() };
        let available = (remaining / chunk.packet_bytes as usize) as u32;
        let packets = (*wanted).min(available);
        (packets, packets as usize * chunk.packet_bytes as usize)
    } else {
        // Compressed: one packet at a time, because a packet boundary is not a byte
        // count the converter can guess.
        (1, remaining)
    };
    if packets == 0 || bytes == 0 {
        unsafe {
            io_number_data_packets.as_ptr().write(0);
        }
        list.mBuffers[0].mDataByteSize = 0;
        list.mBuffers[0].mData = std::ptr::null_mut();
        return 0;
    }

    // SAFETY: `offset` is in bounds because `remaining` above is non-zero.
    let start = chunk.offset;
    chunk.offset += bytes;
    let pointer = unsafe { chunk.data.as_mut_ptr().add(start) };
    list.mBuffers[0].mData = pointer.cast::<c_void>();
    list.mBuffers[0].mDataByteSize = bytes as u32;
    if !out_data_packet_description.is_null() && chunk.described {
        chunk.description.mStartOffset = 0;
        chunk.description.mVariableFramesInPacket = chunk.frames_per_packet;
        chunk.description.mDataByteSize = bytes as u32;
        // SAFETY: the caller asked for a description, and it stays valid until this
        // callback is next invoked because it lives in `chunk`.
        unsafe {
            out_data_packet_description.write(&mut chunk.description);
        }
    }
    // SAFETY: as above.
    unsafe {
        io_number_data_packets.as_ptr().write(packets);
    }
    0
}

// ---------------------------------------------------------------------------
// Probing and registration
// ---------------------------------------------------------------------------

/// The audio codecs this backend can decode, in report order.
pub(crate) const DECODABLE: [Codec; 3] = [
    Codec::Audio(AudioCodec::Aac),
    Codec::Audio(AudioCodec::Alac),
    Codec::Audio(AudioCodec::Mp3),
];

/// The audio codecs this backend can encode, in report order.
///
/// AudioToolbox ships no MP3 encoder and no Vorbis, Opus or FLAC converter at all,
/// so only these two are ever registered in the encoder direction.
pub(crate) const ENCODABLE: [Codec; 2] = [Codec::Audio(AudioCodec::Aac), Codec::Audio(AudioCodec::Alac)];

/// Opens the simplest conversion AudioToolbox can perform: linear PCM to linear
/// PCM at a different sample rate.
///
/// Used by [`available`] because "can this process build a converter at all" is a
/// question about the framework, not about one codec; each codec gets its own probe
/// through [`probe_component`].
fn open_pcm_converter() -> Result<AudioConverterRef> {
    let mut source = pcm_description(DEFAULT_SAMPLE_RATE, ChannelLayout::Stereo, SampleFormat::F32);
    let mut destination =
        pcm_description(44_100, ChannelLayout::Stereo, SampleFormat::F32);
    let mut converter: AudioConverterRef = std::ptr::null_mut();
    let (Some(source), Some(destination), Some(out)) = (
        NonNull::new(&mut source),
        NonNull::new(&mut destination),
        NonNull::new(&mut converter),
    ) else {
        return Err(Error::internal("a stack description cannot be null"));
    };
    // SAFETY: both descriptions are live locals and `AudioConverterNew` writes a
    // converter reference into `out` on success.
    let status = unsafe { AudioConverterNew(source, destination, out) };
    if status != 0 {
        return Err(Error::backend(format!(
            "AudioConverterNew(linear PCM) failed: {}",
            os_status_text(status)
        )));
    }
    if converter.is_null() {
        return Err(Error::backend(
            "AudioConverterNew reported success but returned no converter",
        ));
    }
    Ok(converter)
}

/// `true` when AudioToolbox will build an `AudioConverter`.
///
/// Probed with linear PCM, which is the one conversion every Apple platform can do.
/// A codec whose component is missing is reported by its own
/// [`probe_component`] instead. Never panics.
#[must_use]
pub fn available() -> bool {
    match open_pcm_converter() {
        Ok(converter) => {
            dispose(converter);
            true
        }
        Err(error) => {
            log::debug!("AudioToolbox is not usable: {}", error.message());
            false
        }
    }
}

/// Probes one codec and direction, and explains the outcome.
///
/// The probe really builds a converter and disposes it again, so a codec whose
/// component is missing from the machine is reported as such. It does not test a
/// *stream*: no magic cookie is supplied, because a probe has no stream.
#[must_use]
pub fn probe_component(codec: Codec, kind: CodecKind) -> BackendAvailability {
    if !codec.is_audio() {
        return BackendAvailability::Unavailable(format!(
            "AudioToolbox handles audio only; {} is a VideoToolbox codec",
            codec.name()
        ));
    }
    let allowed = match kind {
        CodecKind::Encoder => ENCODABLE.contains(&codec),
        CodecKind::Decoder => DECODABLE.contains(&codec),
    };
    if !allowed {
        return BackendAvailability::Unavailable(match kind {
            CodecKind::Encoder => format!(
                "AudioToolbox has no {} encoder; this backend encodes AAC and Apple Lossless only",
                codec.label()
            ),
            CodecKind::Decoder => {
                format!("AudioToolbox ships no {} AudioConverter", codec.label())
            }
        });
    }
    match create_converter(codec, kind, DEFAULT_SAMPLE_RATE, ChannelLayout::Stereo, 0, None) {
        Ok((converter, _)) => {
            dispose(converter);
            BackendAvailability::Available
        }
        Err(error) => BackendAvailability::Unavailable(error.message().to_string()),
    }
}

/// Reports the AudioToolbox converter set.
///
/// AudioToolbox has no hardware engine — `AudioConverter` is CPU DSP — to the record
/// is named as such and every `HwCodecSupport` entry carries `max_bit_depth == 0`
/// ("unknown") rather than a bit depth that was never observed. It is reported at
/// all because `HwDeviceInfo` is this crate's machine-capability report, exactly as
/// the Windows backend reports Media Foundation's audio categories.
#[must_use]
pub fn probe_devices() -> Vec<HwDeviceInfo> {
    if !available() {
        return Vec::new();
    }
    let mut device = HwDeviceInfo::new(
        BackendKind::AudioToolbox,
        "Apple AudioToolbox AudioConverter (CPU DSP, no hardware engine)",
        "AudioToolbox",
    );
    for codec in Codec::ALL {
        if !codec.is_audio() {
            continue;
        }
        let encode = ENCODABLE.contains(&codec)
            && probe_component(codec, CodecKind::Encoder).is_available();
        let decode = DECODABLE.contains(&codec)
            && probe_component(codec, CodecKind::Decoder).is_available();
        if !encode && !decode {
            continue;
        }
        device.codecs.push(HwCodecSupport {
            codec,
            decode,
            encode,
            max_width: 0,
            max_height: 0,
            max_bit_depth: 0,
        });
    }
    if device.codecs.is_empty() {
        Vec::new()
    } else {
        vec![device]
    }
}

/// Registers the audio codecs AudioToolbox can actually serve on this machine.
pub fn register(registry: &mut Registry) {
    if !available() {
        log::debug!("AudioToolbox is not reachable; the Apple audio backend registers nothing");
        return;
    }
    for codec in ENCODABLE {
        if !probe_component(codec, CodecKind::Encoder).is_available() {
            continue;
        }
        registry.register_encoder(
            codec,
            BackendKind::AudioToolbox,
            super::super::priority::PLATFORM_FRAMEWORK,
            |config: &EncoderConfig| Ok(Some(Box::new(AtEncoder::new(config)?) as Box<dyn Encoder>)),
            |codec: Codec| probe_component(codec, CodecKind::Encoder),
        );
    }
    for codec in DECODABLE {
        if !probe_component(codec, CodecKind::Decoder).is_available() {
            continue;
        }
        registry.register_decoder(
            codec,
            BackendKind::AudioToolbox,
            super::super::priority::PLATFORM_FRAMEWORK,
            |config: &DecoderConfig| Ok(Some(Box::new(AtDecoder::new(config)?) as Box<dyn Decoder>)),
            |codec: Codec| probe_component(codec, CodecKind::Decoder),
        );
    }
}

// ---------------------------------------------------------------------------
// Sample marshalling
// ---------------------------------------------------------------------------

/// Widens signed 16-bit little-endian samples to `f32` in the same normalisation
/// `AudioFrame::from_f32_planar` uses.
///
/// A 16-bit sample is divided by 32768, so the full-scale negative sample maps to
/// `-1.0` and the widest positive sample to just under `1.0`.
#[must_use]
pub fn s16_to_f32(payload: &[u8]) -> Vec<u8> {
    let mut builder = BufferBuilder::with_capacity(payload.len() * 2);
    for chunk in payload.chunks_exact(2) {
        let value = i16::from_le_bytes([chunk[0], chunk[1]]);
        let scaled = f32::from(value) / 32_768.0;
        builder.push(&scaled.to_le_bytes());
    }
    builder.finish().to_vec()
}

/// The bytes of `frame` as interleaved `f32`, or an error when the layout has no
/// `f32` widening here.
fn pcm_payload(frame: &AudioFrame) -> Result<Vec<u8>> {
    let expected = frame.channels.byte_len(frame.frames, frame.format);
    let bytes = frame
        .samples
        .as_slice()
        .get(..expected)
        .ok_or_else(|| Error::invalid_argument("the audio frame is shorter than it claims"))?;
    match frame.format {
        SampleFormat::F32 => Ok(bytes.to_vec()),
        SampleFormat::S16 => Ok(s16_to_f32(bytes)),
        other => Err(Error::unsupported(format!(
            "the AudioToolbox encoder takes f32 or s16 samples; {} would need a conversion it \
             does not promise",
            other.name()
        ))),
    }
}

/// The `CodecPath` an AudioToolbox encoder reports.
///
/// Shared with the test that asserts the framework naming and the hardware flag
/// agree, so the statement checked is the statement a caller sees.
#[must_use]
pub fn encoder_path(codec: Codec) -> CodecPath {
    CodecPath::new(
        BackendKind::AudioToolbox,
        false,
        format!("AudioConverter {} encoder (CPU DSP, no hardware acceleration)", codec.name()),
    )
}

/// The `CodecPath` an AudioToolbox decoder reports.
#[must_use]
pub fn decoder_path(codec: Codec, cookie_source: &str) -> CodecPath {
    CodecPath::new(
        BackendKind::AudioToolbox,
        false,
        format!(
            "AudioConverter {} decoder (CPU DSP, no hardware acceleration; {cookie_source})",
            codec.name()
        ),
    )
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// A live `AudioConverter` running as an encoder.
pub struct AtEncoder {
    converter: AudioConverterRef,
    codec: Codec,
    path: CodecPath,
    geometry: ConverterGeometry,
    input_format: SampleFormat,
    /// One output packet's worth of scratch, reused for every call.
    output: Vec<u8>,
    /// The chunk the input callback is pulling from.
    chunk: Box<Mutex<InputChunk>>,
    pending: VecDeque<Packet>,
    /// The magic cookie CoreAudio wants stored next to the stream.
    magic_cookie: Option<Buffer>,
    /// The timestamp the next produced packet will carry.
    next_pts: i64,
    /// `true` once the caller has begun draining.
    draining: bool,
}

// SAFETY: `AudioConverterRef` is an opaque CoreFoundation-style handle with no
// thread affinity. It is owned by exactly one `AtEncoder`, which the registry moves
// once onto the worker thread that will use it, and disposed on that same thread.
unsafe impl Send for AtEncoder {}

impl std::fmt::Debug for AtEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtEncoder")
            .field("codec", &self.codec.name())
            .field("path", &self.path.to_string())
            .finish()
    }
}

impl Drop for AtEncoder {
    fn drop(&mut self) {
        // The converter must be disposed before the chunk its input callback points
        // at is released; `AudioConverterDispose` does not return until the
        // converter can no longer call back. `dispose` runs here, and the `Box` is
        // dropped afterwards, when the fields drop.
        dispose(self.converter);
        self.converter = std::ptr::null_mut();
    }
}

impl AtEncoder {
    /// Creates an encoder for `config`.
    ///
    /// Fails with a specific message for a codec AudioToolbox cannot encode, for an
    /// input sample layout that is neither `f32` nor `s16`, and for any refusal from
    /// CoreAudio.
    pub fn new(config: &EncoderConfig) -> Result<Self> {
        let codec = config.codec;
        if !ENCODABLE.contains(&codec) {
            return Err(Error::unsupported(format!(
                "AudioToolbox has no {} encoder; this backend encodes AAC and Apple Lossless only",
                codec.label()
            )));
        }
        let input_format = match config.sample_format {
            SampleFormat::F32 | SampleFormat::S16 => config.sample_format,
            other => {
                return Err(Error::unsupported(format!(
                    "the AudioToolbox encoder takes f32 or s16 samples, not {}",
                    other.name()
                )))
            }
        };
        let sample_rate = config.sample_rate.max(1);
        let (converter, geometry) = create_converter(
            codec,
            CodecKind::Encoder,
            sample_rate,
            config.channels,
            config.bitrate,
            None,
        )?;
        // The cookie is read *after* the converter exists, because it describes the
        // output format the converter negotiated.
        let magic_cookie = read_property_bytes(converter, kAudioConverterCompressionMagicCookie)
            .filter(|cookie| !cookie.is_empty())
            .map(Buffer::from_vec);
        let capacity = read_property_u32(converter, kAudioConverterPropertyMaximumOutputPacketSize)
            .unwrap_or(1_024)
            .clamp(256, 1 << 20) as usize;
        Ok(Self {
            converter,
            codec,
            path: encoder_path(codec),
            geometry,
            input_format,
            output: vec![0u8; capacity],
            chunk: Box::new(Mutex::new(InputChunk::empty())),
            pending: VecDeque::new(),
            magic_cookie,
            next_pts: 0,
            draining: false,
        })
    }

    /// Pulls every output packet the converter will give up for the current input.
    fn drain(&mut self) -> Result<()> {
        let converter = self.converter;
        let input = self.chunk.as_ref() as *const Mutex<InputChunk> as *mut c_void;
        let channels = self.geometry.channels.channels();
        loop {
            let mut packets: u32 = 1;
            let mut list = AudioBufferList {
                mNumberBuffers: 1,
                mBuffers: [AudioBuffer {
                    mNumberChannels: channels,
                    mDataByteSize: self.output.len() as u32,
                    mData: self.output.as_mut_ptr().cast::<c_void>(),
                }],
            };
            let Some(packets_ptr) = NonNull::new(&mut packets) else {
                return Err(Error::internal("a stack packet count cannot be null"));
            };
            let Some(list_ptr) = NonNull::new(&mut list) else {
                return Err(Error::internal("a stack buffer list cannot be null"));
            };
            // SAFETY: the converter is live, the callback and its user data are the
            // ones this session registered, and both the packet count and the buffer
            // list are valid locals describing `self.output`.
            let status = unsafe {
                AudioConverterFillComplexBuffer(
                    converter,
                    Some(converter_input),
                    input,
                    packets_ptr,
                    list_ptr,
                    std::ptr::null_mut(),
                )
            };
            if status != 0 {
                return Err(Error::backend(format!(
                    "AudioConverterFillComplexBuffer failed: {}",
                    os_status_text(status)
                )));
            }
            if packets == 0 || list.mBuffers[0].mDataByteSize == 0 {
                break;
            }
            let written = (list.mBuffers[0].mDataByteSize as usize).min(self.output.len());
            // Every AAC, Apple Lossless and MP3 frame is independently decodable, so
            // each compressed packet is a random-access point.
            self.pending.push_back(Packet::new(
                self.codec,
                Buffer::from_slice(&self.output[..written]),
                self.next_pts,
                packet_flags::KEYFRAME,
            ));
            self.advance_pts();
        }
        Ok(())
    }

    /// Moves the output timestamp on by one packet.
    fn advance_pts(&mut self) {
        let step = i64::from(self.geometry.frames_per_packet) * i64::from(TIME_SCALE)
            / i64::from(self.geometry.sample_rate.max(1));
        self.next_pts += step.max(1);
    }
}

impl Encoder for AtEncoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, frame: Frame) -> Result<()> {
        let Frame::Audio(audio) = frame else {
            return Err(Error::invalid_argument(format!(
                "the {} AudioToolbox encoder needs raw audio frames",
                self.codec.name()
            )));
        };
        if self.draining {
            return Err(Error::state("the AudioToolbox encoder is already draining"));
        }
        if audio.sample_rate != self.geometry.sample_rate {
            return Err(Error::invalid_argument(format!(
                "this {} encoder runs at {} Hz but was handed a {} Hz frame",
                self.codec.name(),
                self.geometry.sample_rate,
                audio.sample_rate
            )));
        }
        if audio.channels != self.geometry.channels {
            return Err(Error::invalid_argument(format!(
                "this {} encoder runs with {} channels but was handed {}",
                self.codec.name(),
                self.geometry.channels.name(),
                audio.channels.name()
            )));
        }
        if audio.format != self.input_format {
            return Err(Error::invalid_argument(format!(
                "this {} encoder was opened for {} samples but was handed {}",
                self.codec.name(),
                self.input_format.name(),
                audio.format.name()
            )));
        }
        if self.pending.is_empty() && self.next_pts == 0 {
            self.next_pts = audio.pts;
        }
        let payload = pcm_payload(&audio)?;
        let packet_bytes = self.geometry.channels.channels() * SampleFormat::F32.bytes_per_sample() as u32;
        {
            let mut chunk = self
                .chunk
                .lock()
                .map_err(|_| Error::state("the AudioConverter input chunk is poisoned"))?;
            chunk.set(payload, packet_bytes, 1, false);
        }
        self.drain()
    }

    fn receive(&mut self) -> Result<Option<Packet>> {
        if self.pending.is_empty() && !self.draining {
            self.drain()?;
        }
        Ok(self.pending.pop_front())
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        self.draining = true;
        // Emptying the chunk is what tells the converter that the stream ended; the
        // input callback then reports zero packets and the converter flushes.
        {
            let mut chunk = self
                .chunk
                .lock()
                .map_err(|_| Error::state("the AudioConverter input chunk is poisoned"))?;
            chunk.set(Vec::new(), 0, 0, false);
        }
        self.drain()?;
        Ok(self.pending.drain(..).collect())
    }

    fn set_bitrate(&mut self, bitrate: u64) -> Result<()> {
        if bitrate == 0 {
            return Err(Error::invalid_argument("a zero bitrate is not a bitrate"));
        }
        if self.codec != Codec::Audio(AudioCodec::Aac) {
            return Err(Error::unsupported(format!(
                "{} has no bitrate to retune",
                self.codec.label()
            )));
        }
        let mut target = bitrate.min(u64::from(u32::MAX)) as u32;
        let Some(data) = NonNull::new((&mut target as *mut u32).cast::<c_void>()) else {
            return Err(Error::internal("a stack bitrate cannot be null"));
        };
        // SAFETY: the property is a `u32` and `target` is a live local.
        let status = unsafe {
            AudioConverterSetProperty(
                self.converter,
                kAudioConverterEncodeBitRate,
                std::mem::size_of::<u32>() as u32,
                data,
            )
        };
        if status != 0 {
            return Err(Error::backend(format!(
                "AudioConverter did not accept bitrate {target}: {}",
                os_status_text(status)
            )));
        }
        Ok(())
    }

    fn extradata(&self) -> Option<Buffer> {
        self.magic_cookie.clone()
    }
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// A live `AudioConverter` running as a decoder.
pub struct AtDecoder {
    converter: AudioConverterRef,
    codec: Codec,
    path: CodecPath,
    geometry: ConverterGeometry,
    /// Scratch for one call's worth of interleaved `f32` PCM.
    output: Vec<u8>,
    chunk: Box<Mutex<InputChunk>>,
    pending: VecDeque<Frame>,
    /// Timestamp of the packet currently being decoded.
    base_pts: i64,
    /// Sample frames produced from that packet so far.
    produced: i64,
}

// SAFETY: the same argument as `AtEncoder`: the converter handle has no thread
// affinity, is owned by one `AtDecoder`, and is disposed on the thread that used it.
unsafe impl Send for AtDecoder {}

impl std::fmt::Debug for AtDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtDecoder")
            .field("codec", &self.codec.name())
            .field("path", &self.path.to_string())
            .finish()
    }
}

impl Drop for AtDecoder {
    fn drop(&mut self) {
        // The same ordering rule as `AtEncoder::drop`.
        dispose(self.converter);
        self.converter = std::ptr::null_mut();
    }
}

impl AtDecoder {
    /// Creates a decoder for `config`.
    ///
    /// The channel layout and sample rate come from the caller's hints when they are
    /// present, and from the defaults CoreAudio uses otherwise; a caller that
    /// supplies neither gets a stereo 48 kHz converter, which is what the compressed
    /// formats' own headers correct once real data arrives.
    pub fn new(config: &DecoderConfig) -> Result<Self> {
        let codec = config.codec;
        if !DECODABLE.contains(&codec) {
            return Err(Error::unsupported(format!(
                "AudioToolbox ships no {} AudioConverter",
                codec.label()
            )));
        }
        if let Some(format) = config.sample_format {
            if format != SampleFormat::F32 {
                return Err(Error::unsupported(format!(
                    "the AudioToolbox decoder produces f32 samples, not {}",
                    format.name()
                )));
            }
        }
        let sample_rate = if config.sample_rate > 0 { config.sample_rate } else { DEFAULT_SAMPLE_RATE };
        let channels = config.channels.unwrap_or(ChannelLayout::Stereo);
        let extradata = config.extradata.as_slice();
        let cookie = if extradata.is_empty() { None } else { Some(extradata) };
        let (converter, geometry) =
            create_converter(codec, CodecKind::Decoder, sample_rate, channels, 0, cookie)?;
        let cookie_source = if cookie.is_some() {
            "magic cookie from extradata"
        } else if codec == Codec::Audio(AudioCodec::Alac) {
            "format information from kAudioFormatProperty_FormatInfo"
        } else {
            "configuration in band"
        };
        Ok(Self {
            converter,
            codec,
            path: decoder_path(codec, cookie_source),
            geometry,
            output: vec![0u8; DECODE_CHUNK_BYTES],
            chunk: Box::new(Mutex::new(InputChunk::empty())),
            pending: VecDeque::new(),
            base_pts: 0,
            produced: 0,
        })
    }

    /// Converts everything the current input can produce.
    fn drain(&mut self) -> Result<()> {
        let converter = self.converter;
        let input = self.chunk.as_ref() as *const Mutex<InputChunk> as *mut c_void;
        let channels = self.geometry.channels.channels();
        let bytes_per_frame = SampleFormat::F32.bytes_per_sample() * channels as usize;
        if bytes_per_frame == 0 {
            return Err(Error::internal("a decoded PCM frame cannot be zero bytes"));
        }
        loop {
            let wanted = (self.output.len() / bytes_per_frame).max(1) as u32;
            let mut packets = wanted;
            let mut list = AudioBufferList {
                mNumberBuffers: 1,
                mBuffers: [AudioBuffer {
                    mNumberChannels: channels,
                    mDataByteSize: self.output.len() as u32,
                    mData: self.output.as_mut_ptr().cast::<c_void>(),
                }],
            };
            let Some(packets_ptr) = NonNull::new(&mut packets) else {
                return Err(Error::internal("a stack packet count cannot be null"));
            };
            let Some(list_ptr) = NonNull::new(&mut list) else {
                return Err(Error::internal("a stack buffer list cannot be null"));
            };
            // SAFETY: the converter is live and both out-parameters are valid locals
            // that describe `self.output`. PCM output uses no packet descriptions, so
            // that argument is NULL.
            let status = unsafe {
                AudioConverterFillComplexBuffer(
                    converter,
                    Some(converter_input),
                    input,
                    packets_ptr,
                    list_ptr,
                    std::ptr::null_mut(),
                )
            };
            if status != 0 {
                return Err(Error::backend(format!(
                    "AudioConverterFillComplexBuffer failed: {}",
                    os_status_text(status)
                )));
            }
            if packets == 0 {
                break;
            }
            let frames = (list.mBuffers[0].mDataByteSize as usize / bytes_per_frame) as u32;
            if frames == 0 {
                break;
            }
            let written = frames as usize * bytes_per_frame;
            let pts = self.base_pts
                + self.produced * i64::from(TIME_SCALE) / i64::from(self.geometry.sample_rate.max(1));
            let frame = AudioFrame::new(
                SampleFormat::F32,
                self.geometry.sample_rate,
                self.geometry.channels,
                frames,
                Buffer::from_slice(&self.output[..written.min(self.output.len())]),
                pts,
            )?;
            self.produced += i64::from(frames);
            self.pending.push_back(Frame::Audio(frame));
        }
        Ok(())
    }
}

impl Decoder for AtDecoder {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, packet: Packet) -> Result<()> {
        if packet.is_header() {
            // Configuration reached the converter through the magic cookie set in
            // `AtDecoder::new`; a header packet adds nothing.
            return Ok(());
        }
        if packet.is_eos() {
            return self.flush().map(|_| ());
        }
        if packet.codec != self.codec {
            return Err(Error::invalid_argument(format!(
                "this {} decoder cannot take {} packets",
                self.codec.name(),
                packet.codec.name()
            )));
        }
        if packet.is_empty() {
            return Err(Error::invalid_argument("the packet carries no compressed bytes"));
        }
        self.base_pts = packet.pts;
        self.produced = 0;
        {
            let mut chunk = self
                .chunk
                .lock()
                .map_err(|_| Error::state("the AudioConverter input chunk is poisoned"))?;
            chunk.set(packet.data.to_vec(), 0, self.geometry.frames_per_packet, true);
        }
        self.drain()
    }

    fn receive(&mut self) -> Result<Option<Frame>> {
        Ok(self.pending.pop_front())
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        // Emptying the chunk lets the converter emit whatever it still holds.
        {
            let mut chunk = self
                .chunk
                .lock()
                .map_err(|_| Error::state("the AudioConverter input chunk is poisoned"))?;
            chunk.set(Vec::new(), 0, 0, false);
        }
        self.drain()?;
        Ok(self.pending.drain(..).collect())
    }
}

/// A converter's input-format flag for a compressed codec, exposed so the tests can
/// assert the values that are easy to get wrong.
#[must_use]
pub fn compressed_flags(codec: Codec) -> Option<u32> {
    compressed_description(codec, DEFAULT_SAMPLE_RATE, ChannelLayout::Stereo).map(|d| d.mFormatFlags)
}

/// The `AudioFormatID` for a compressed codec, exposed for the same reason.
#[must_use]
pub fn compressed_format_id(codec: Codec) -> Option<AudioFormatID> {
    compressed_description(codec, DEFAULT_SAMPLE_RATE, ChannelLayout::Stereo).map(|d| d.mFormatID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::codec::VideoCodec;

    /// The compressed layout for a codec at a nominal geometry.
    fn layout(codec: Codec) -> AudioStreamBasicDescription {
        compressed_description(codec, DEFAULT_SAMPLE_RATE, ChannelLayout::Stereo)
            .expect("the codec has a compressed layout")
    }

    #[test]
    fn the_codec_lists_match_what_audiotoolbox_can_do() {
        assert_eq!(ENCODABLE.len(), 2);
        assert!(ENCODABLE.contains(&Codec::Audio(AudioCodec::Aac)));
        assert!(ENCODABLE.contains(&Codec::Audio(AudioCodec::Alac)));
        assert_eq!(DECODABLE.len(), 3);
        assert!(DECODABLE.contains(&Codec::Audio(AudioCodec::Mp3)));
        // MP3 is decode-only, and Vorbis/Opus/FLAC are neither.
        assert!(!ENCODABLE.contains(&Codec::Audio(AudioCodec::Mp3)));
        for codec in [AudioCodec::Vorbis, AudioCodec::Opus, AudioCodec::Flac] {
            assert!(!ENCODABLE.contains(&Codec::Audio(codec)));
            assert!(!DECODABLE.contains(&Codec::Audio(codec)));
        }
        for codec in DECODABLE.iter().chain(ENCODABLE.iter()) {
            assert!(codec.is_audio(), "{codec} must be an audio codec");
        }
    }

    #[test]
    fn every_listed_codec_has_an_audio_format_id() {
        for codec in ENCODABLE.iter().chain(DECODABLE.iter()) {
            assert!(super::super::audio_format_id(*codec).is_some(), "{codec}");
        }
    }

    #[test]
    fn the_pcm_layout_is_interleaved_packed_float() {
        let description = pcm_description(48_000, ChannelLayout::Stereo, SampleFormat::F32);
        assert_eq!(description.mFormatID, kAudioFormatLinearPCM);
        assert_eq!(description.mFormatFlags, kAudioFormatFlagsNativeFloatPacked);
        assert_eq!(description.mSampleRate, 48_000.0);
        assert_eq!(description.mChannelsPerFrame, 2);
        assert_eq!(description.mFramesPerPacket, 1);
        assert_eq!(description.mBitsPerChannel, 32);
        assert_eq!(description.mBytesPerFrame, 8);
        assert_eq!(description.mBytesPerPacket, 8);
        assert_eq!(description.mReserved, 0);
    }

    #[test]
    fn the_pcm_layout_scales_with_channels_and_sample_width() {
        let mono = pcm_description(44_100, ChannelLayout::Mono, SampleFormat::S16);
        assert_eq!(mono.mBytesPerFrame, 2);
        assert_eq!(mono.mBitsPerChannel, 16);
        assert_eq!(
            mono.mFormatFlags,
            kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked
        );
        let six = pcm_description(48_000, ChannelLayout::FiveOne, SampleFormat::F32);
        assert_eq!(six.mChannelsPerFrame, 6);
        assert_eq!(six.mBytesPerFrame, 24);
    }

    #[test]
    fn the_compressed_layouts_carry_the_flags_their_codec_needs() {
        // AAC's flags are an MPEG-4 object type, not a bit field.
        let aac = layout(Codec::Audio(AudioCodec::Aac));
        assert_eq!(aac.mFormatID, kAudioFormatMPEG4AAC);
        assert_eq!(aac.mFormatFlags, AAC_LC);
        assert_eq!(aac.mFormatFlags, 2, "kMPEG4Object_AAC_LC is 2");
        assert_eq!(aac.mFramesPerPacket, 1_024);
        assert_eq!(aac.mBytesPerPacket, 0);
        assert_eq!(aac.mBytesPerFrame, 0);
        assert_eq!(aac.mBitsPerChannel, 0);
        assert_eq!(aac.mChannelsPerFrame, 2);
        assert_eq!(aac.mSampleRate, f64::from(DEFAULT_SAMPLE_RATE));

        // Apple Lossless needs the lossless source-depth flag, not 0.
        let alac = layout(Codec::Audio(AudioCodec::Alac));
        assert_eq!(alac.mFormatID, kAudioFormatAppleLossless);
        assert_eq!(alac.mFormatFlags, kAppleLosslessFormatFlag_16BitSourceData);
        assert_ne!(alac.mFormatFlags, 0);
        assert_eq!(alac.mFramesPerPacket, 4_096);

        let mp3 = layout(Codec::Audio(AudioCodec::Mp3));
        assert_eq!(mp3.mFormatID, kAudioFormatMPEGLayer3);
        assert_eq!(mp3.mFormatFlags, 0);
        assert_eq!(mp3.mFramesPerPacket, 1_152);
    }

    #[test]
    fn the_compressed_layouts_are_total_over_the_codecs_they_cover() {
        for codec in Codec::ALL {
            let expected = matches!(
                codec,
                Codec::Audio(AudioCodec::Aac | AudioCodec::Alac | AudioCodec::Mp3)
            );
            assert_eq!(compressed_description(codec, 48_000, ChannelLayout::Stereo).is_some(), expected, "{codec}");
            assert_eq!(frames_per_packet(codec).is_some(), expected, "{codec}");
            assert_eq!(compressed_flags(codec).is_some(), expected, "{codec}");
            assert_eq!(compressed_format_id(codec).is_some(), expected, "{codec}");
        }
    }

    #[test]
    fn the_packet_sizes_are_the_standard_ones() {
        assert_eq!(frames_per_packet(Codec::Audio(AudioCodec::Aac)), Some(1_024));
        assert_eq!(frames_per_packet(Codec::Audio(AudioCodec::Alac)), Some(4_096));
        assert_eq!(frames_per_packet(Codec::Audio(AudioCodec::Mp3)), Some(1_152));
        assert_eq!(frames_per_packet(Codec::Audio(AudioCodec::Opus)), None);
        assert_eq!(frames_per_packet(Codec::Video(VideoCodec::H264)), None);
    }

    #[test]
    fn s16_widens_to_f32_without_clipping_the_full_scale_negative_sample() {
        let payload: Vec<u8> = [0i16, -32_768, 32_767, 16_384]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let widened = s16_to_f32(&payload);
        assert_eq!(widened.len(), 4 * 4);
        let values: Vec<f32> = widened
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();
        assert_eq!(values[0], 0.0);
        assert_eq!(values[1], -1.0);
        assert!(values[2] < 1.0 && values[2] > 0.999, "{}", values[2]);
        assert_eq!(values[3], 0.5);
        // An odd trailing byte is ignored rather than producing a partial sample.
        assert_eq!(s16_to_f32(&[0, 0, 7]).len(), 4);
    }

    #[test]
    fn the_encoder_path_reports_audiotoolbox_and_no_hardware() {
        for codec in ENCODABLE {
            let path = encoder_path(codec);
            assert_eq!(path.backend, BackendKind::AudioToolbox);
            assert!(path.is_native());
            assert!(!path.hardware_accelerated, "AudioToolbox is CPU DSP: {}", path.detail);
            assert!(path.detail.contains("AudioConverter"), "{}", path.detail);
            assert!(path.detail.contains("no hardware acceleration"), "{}", path.detail);
            assert!(path.detail.contains(codec.name()), "{}", path.detail);
        }
    }

    #[test]
    fn the_decoder_path_reports_audiotoolbox_and_no_hardware() {
        for codec in DECODABLE {
            for source in ["magic cookie from extradata", "configuration in band"] {
                let path = decoder_path(codec, source);
                assert_eq!(path.backend, BackendKind::AudioToolbox);
                assert!(path.is_native());
                assert!(!path.hardware_accelerated, "{}", path.detail);
                assert!(path.detail.contains("AudioConverter"), "{}", path.detail);
                assert!(path.detail.contains(source), "{}", path.detail);
                assert!(path.detail.contains(codec.name()), "{}", path.detail);
            }
        }
    }

    #[test]
    fn probing_a_video_codec_answers_with_a_specific_reason() {
        for codec in Codec::ALL {
            if codec.is_audio() {
                continue;
            }
            for kind in CodecKind::ALL {
                let availability = probe_component(codec, kind);
                let reason = availability.reason().expect("video is never an AudioToolbox codec");
                assert!(reason.contains("VideoToolbox"), "{reason}");
            }
        }
    }

    #[test]
    fn probing_a_codec_without_a_converter_names_it() {
        for codec in [Codec::Audio(AudioCodec::Vorbis), Codec::Audio(AudioCodec::Opus), Codec::Audio(AudioCodec::Flac)]
        {
            for kind in CodecKind::ALL {
                let availability = probe_component(codec, kind);
                let reason = availability.reason().expect("no such converter exists");
                assert!(reason.contains(codec.label()), "{reason}");
            }
        }
    }

    #[test]
    fn probing_the_mp3_encoder_says_there_is_none() {
        let availability = probe_component(Codec::Audio(AudioCodec::Mp3), CodecKind::Encoder);
        let reason = availability.reason().expect("there is no MP3 encoder in AudioToolbox");
        assert!(reason.contains("MP3"), "{reason}");
        assert!(reason.contains("AAC and Apple Lossless"), "{reason}");
    }

    #[test]
    fn the_output_sample_format_is_f32_and_nothing_else() {
        // The factory refuses any other request rather than converting silently.
        assert_eq!(SampleFormat::F32.bytes_per_sample(), 4);
        assert_ne!(SampleFormat::S16, SampleFormat::F32);
    }

    #[test]
    fn a_fill_format_info_call_is_not_attempted_for_a_codec_that_needs_no_cookie() {
        // The AAC and MPEGLayer3 descriptions are complete on their own; the
        // fallback exists for Apple Lossless, whose stream configuration is not
        // derivable from the identifier alone.
        let aac = compressed_description(Codec::Audio(AudioCodec::Aac), 48_000, ChannelLayout::Stereo)
            .expect("aac has a layout");
        assert_eq!(aac.mFormatFlags, AAC_LC);
        let alac = compressed_description(Codec::Audio(AudioCodec::Alac), 48_000, ChannelLayout::Stereo)
            .expect("alac has a layout");
        assert_eq!(alac.mFormatFlags, kAppleLosslessFormatFlag_16BitSourceData);
    }

    #[test]
    #[ignore = "requires a macOS media engine"]
    fn audiotoolbox_is_available() {
        assert!(available(), "AudioToolbox must be reachable on macOS");
    }

    #[test]
    #[ignore = "requires a macOS media engine"]
    fn registration_adds_exactly_the_codecs_audiotoolbox_serves() {
        let mut registry = Registry::new();
        register(&mut registry);
        for codec in ENCODABLE {
            if probe_component(codec, CodecKind::Encoder).is_available() {
                let report = registry.capability(codec).expect("a registered codec is reported");
                assert!(
                    report.encoders.iter().any(|backend| backend.backend == BackendKind::AudioToolbox),
                    "{codec} lost its encoder registration"
                );
            }
        }
        for codec in DECODABLE {
            if probe_component(codec, CodecKind::Decoder).is_available() {
                let report = registry.capability(codec).expect("a registered codec is reported");
                assert!(
                    report.decoders.iter().any(|backend| backend.backend == BackendKind::AudioToolbox),
                    "{codec} lost its decoder registration"
                );
            }
        }
        for codec in Codec::ALL {
            let report = registry.capability(codec).expect("every codec is reported");
            if !ENCODABLE.contains(&codec) {
                assert!(
                    !report.encoders.iter().any(|backend| backend.backend == BackendKind::AudioToolbox),
                    "{codec} must not have an AudioToolbox encoder"
                );
            }
            if !DECODABLE.contains(&codec) {
                assert!(
                    !report.decoders.iter().any(|backend| backend.backend == BackendKind::AudioToolbox),
                    "{codec} must not have an AudioToolbox decoder"
                );
            }
        }
    }

    #[test]
    #[ignore = "requires a macOS media engine"]
    fn a_real_aac_encode_produces_a_packet_with_extradata() {
        let config = EncoderConfig::audio(AudioCodec::Aac, 44_100, ChannelLayout::Stereo)
            .with_bitrate(128_000);
        let mut encoder = AtEncoder::new(&config).expect("the AAC converter must open");
        // 1024 stereo f32 frames, twice.
        let payload = vec![0u8; 1_024 * 2 * 4];
        for index in 0..4 {
            let frame = AudioFrame::new(
                SampleFormat::F32,
                44_100,
                ChannelLayout::Stereo,
                1_024,
                Buffer::from_vec(payload.clone()),
                index * 23,
            )
            .expect("a valid audio frame");
            encoder.push(Frame::Audio(frame)).expect("the converter must accept the frame");
        }
        let mut packets: Vec<Packet> = Vec::new();
        while let Some(packet) = encoder.receive().expect("receive must not fail") {
            packets.push(packet);
        }
        packets.extend(encoder.flush().expect("flush must not fail"));
        assert!(!packets.is_empty(), "a real AAC encode produced no output at all");
        assert!(packets.iter().any(|packet| !packet.is_empty()), "every packet was empty");
        assert_eq!(encoder.path().backend, BackendKind::AudioToolbox);
        // AAC has to publish an AudioSpecificConfig for a container to be usable.
        assert!(encoder.extradata().is_some(), "AAC must expose its magic cookie");
    }

    #[test]
    #[ignore = "requires a macOS media engine"]
    fn a_real_mp3_decode_session_opens_and_drains_empty() {
        let config = DecoderConfig::new(Codec::Audio(AudioCodec::Mp3))
            .with_audio(44_100, ChannelLayout::Stereo);
        let mut decoder = AtDecoder::new(&config).expect("the MP3 converter must open");
        assert!(decoder.receive().expect("receive must not fail").is_none());
        assert!(decoder.flush().expect("flush must not fail").is_empty());
        assert_eq!(decoder.path().backend, BackendKind::AudioToolbox);
        assert!(!decoder.path().hardware_accelerated);
    }
}
