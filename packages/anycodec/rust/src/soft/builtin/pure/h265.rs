//! Pure-Rust **H.265/HEVC** decoding, on [`rusty_h265`].
//!
//! ## Input
//!
//! HEVC is carried as **Annex-B** (`00 00 01` start codes) — the shape of an
//! elementary stream, RTP payloads and a transport stream. That is the only
//! shape this decoder accepts, and a packet that does not contain a start code
//! is rejected with a message saying so rather than being silently ignored.
//! (An MP4/MKV carriage with `lengthSizeMinusOne` prefixes would need the
//! container's prefix width, which is not part of the "bare codec bitstream"
//! this tier is given.)
//!
//! Parameter sets are taken from `extradata` when it is an `hvcC` record or an
//! Annex-B blob, and otherwise from the stream itself: VPS/SPS/PPS are ordinary
//! NAL units, so a stream that carries them in band needs no `extradata`.
//!
//! `rusty_h265` covers Main, Main 10 and Main Still Picture — 4:2:0 at 8 and 10
//! bits. Range extensions, screen-content coding and layered profiles are
//! refused by the crate itself with a named `Error::Unsupported`, which this
//! module surfaces as [`crate::error::ErrorKind::Unsupported`].
//!
//! ## What is *not* claimed
//!
//! * [`VideoFrame::keyframe`] is **always `false`**: the crate reports pictures
//!   in output order and does not expose IRAP boundaries through its frame type.
//!   `false` is the safe direction — it never lets a caller skip a picture it
//!   still needs.
//! * [`VideoFrame::pts`] is the `pts` of the packet that completed the access
//!   unit when one was supplied, and an output ordinal otherwise.
//! * 10-bit pictures come back as [`PixelFormat::P010`], with the sample values
//!   in the low bits of each little-endian 16-bit word, exactly as `rusty_h265`
//!   stores them.

use crate::engine::backend::{BackendKind, CodecPath};
use crate::engine::traits::Decoder;
use crate::engine::{BackendAvailability, Registry};
use crate::error::{Error, ErrorKind, Result};
use crate::media::codec::{Codec, VideoCodec};
use crate::media::config::DecoderConfig;
use crate::media::frame::Frame;
use crate::media::packet::Packet;
use crate::native::bitstream::{annex_b_parameter_sets, has_start_code};

use crate::soft::PRIORITY_BUILTIN;
use super::{i420_frame, p010_frame, probe, Direction};

/// The one codec this module serves.
pub(crate) const CODEC: Codec = Codec::Video(VideoCodec::H265);

/// The crate that decodes it.
pub(crate) const DECODER_CRATE: &str = "rusty_h265";

/// A pure-Rust HEVC decoder.
pub struct H265Decoder {
    path: CodecPath,
    inner: rusty_h265::Decoder,
    /// `DecoderConfig::annex_b`, i.e. "the caller promised start codes".
    annex_b: bool,
    pending: Vec<Frame>,
    pts_cursor: i64,
}

impl std::fmt::Debug for H265Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H265Decoder")
            .field("path", &self.path.to_string())
            .field("annex_b", &self.annex_b)
            .field("queued", &self.pending.len())
            .finish()
    }
}

impl H265Decoder {
    /// Builds a decoder from `config`.
    ///
    /// Fails with [`crate::error::ErrorKind::Unsupported`] for any codec other than H.265 and with
    /// [`crate::error::ErrorKind::InvalidArgument`] when `extradata` is present but neither an
    /// `hvcC` record nor a start-code-prefixed parameter-set blob, or when it carries a parameter
    /// set the decoder rejects.
    pub fn new(config: &DecoderConfig) -> Result<Self> {
        if config.codec != CODEC {
            return Err(Error::unsupported(format!(
                "the {DECODER_CRATE} decoder produces h265 only, not {}",
                config.codec
            )));
        }
        let mut decoder = Self {
            path: CodecPath::new(
                BackendKind::Rust,
                false,
                format!("rust: {DECODER_CRATE} decoder"),
            ),
            inner: rusty_h265::Decoder::new(),
            annex_b: config.annex_b,
            pending: Vec::new(),
            pts_cursor: 0,
        };
        if !config.extradata.is_empty() {
            decoder.feed_parameter_sets(config.extradata.as_slice())?;
        }
        Ok(decoder)
    }

    /// `true` when the packets are expected to carry start codes.
    pub const fn is_annex_b(&self) -> bool {
        self.annex_b
    }

    /// Feeds an `hvcC` record or a start-code-prefixed parameter-set blob.
    fn feed_parameter_sets(&mut self, blob: &[u8]) -> Result<()> {
        let sets = annex_b_parameter_sets(CODEC, blob, self.annex_b);
        if sets.is_empty() {
            return Err(Error::invalid_argument(format!(
                "the {DECODER_CRATE} decoder was given {} bytes of extradata that are neither an \
                 `hvcC` record nor start-code-prefixed parameter sets",
                blob.len()
            )));
        }
        for set in &sets {
            // `annex_b_parameter_sets` prefixes an `hvcC`'s NAL units with a
            // start code but returns the units of an Annex-B blob with the code
            // stripped, and `push_annexb` only understands start codes — so
            // normalise here instead of depending on that difference.
            let unit = with_start_code(set);
            let outcome = self.inner.push_annexb(&unit, None);
            if let Err(error) = outcome {
                return Err(Error::invalid_argument(format!(
                    "the {DECODER_CRATE} decoder rejected a parameter set from the {} bytes of \
                     decoder configuration: {error}",
                    blob.len()
                )));
            }
        }
        Ok(())
    }

    /// Decodes every picture the last push made available.
    fn drain(&mut self) -> Result<()> {
        loop {
            match self.inner.next_frame() {
                Ok(frame) => {
                    let pts = frame.pts.unwrap_or(self.pts_cursor);
                    self.pts_cursor = pts + 1;
                    let picture = picture(&frame, pts)?;
                    self.pending.push(Frame::Video(picture));
                }
                Err(rusty_h265::Error::Again) | Err(rusty_h265::Error::Eof) => return Ok(()),
                Err(rusty_h265::Error::Unsupported(detail)) => {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        format!("{DECODER_CRATE} cannot decode this hevc stream: {detail}"),
                    ))
                }
                Err(error) => {
                    return Err(Error::backend(format!("{DECODER_CRATE} decode failed: {error}")))
                }
            }
        }
    }
}

impl Decoder for H265Decoder {
    fn codec(&self) -> Codec {
        CODEC
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, packet: Packet) -> Result<()> {
        if packet.is_eos() || packet.is_empty() {
            return Ok(());
        }
        if packet.is_header() {
            return self.feed_parameter_sets(packet.data.as_slice());
        }
        if !self.annex_b && !has_start_code(packet.data.as_slice()) {
            return Err(Error::invalid_argument(format!(
                "the {DECODER_CRATE} decoder needs start-code-delimited NAL units; a {}-byte \
                 packet contained no `00 00 01` start code (set `DecoderConfig::annex_b` if this \
                 stream really is Annex-B, or carry the container's length prefixes instead)",
                packet.len()
            )));
        }
        // Parameter sets and slices may both be in the packet. `push_annexb`
        // reports the first damaged unit but keeps everything it did parse, and
        // the pictures that decoded stay queued, so the error is reported
        // without losing work.
        let outcome = self.inner.push_annexb(packet.data.as_slice(), Some(packet.pts));
        self.drain()?;
        match outcome {
            Ok(()) => Ok(()),
            Err(error) => {
                Err(Error::backend(format!("{DECODER_CRATE} rejected a NAL unit: {error}")))
            }
        }
    }

    fn receive(&mut self) -> Result<Option<Frame>> {
        Ok(if self.pending.is_empty() { None } else { Some(self.pending.remove(0)) })
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        self.inner.flush();
        self.drain()?;
        Ok(std::mem::take(&mut self.pending))
    }
}

/// A NAL unit carrying a four-byte Annex-B start code, whatever shape it
/// arrived in.
fn with_start_code(unit: &[u8]) -> Vec<u8> {
    if unit.starts_with(&[0, 0, 0, 1]) {
        return unit.to_vec();
    }
    let mut out = Vec::with_capacity(unit.len() + 4);
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(unit);
    out
}

/// Converts a decoded HEVC picture into the AnyCodec media model.
fn picture(frame: &rusty_h265::Frame, pts: i64) -> Result<crate::media::frame::VideoFrame> {
    let source = &frame.picture;
    if source.chroma_format_idc != 1 {
        return Err(Error::unsupported(format!(
            "the {DECODER_CRATE} decoder produced chroma_format_idc {}, which is not 4:2:0",
            source.chroma_format_idc
        )));
    }
    let width = u32::try_from(frame.width)
        .map_err(|_| Error::backend("the hevc picture is wider than u32::MAX"))?;
    let height = u32::try_from(frame.height)
        .map_err(|_| Error::backend("the hevc picture is taller than u32::MAX"))?;
    if width == 0 || height == 0 {
        return Err(Error::backend("the hevc decoder produced an empty picture"));
    }
    let (crop_x, crop_y, crop_width, crop_height) = source.crop;
    if crop_width < frame.width || crop_height < frame.height {
        return Err(Error::backend(format!(
            "the hevc crop rectangle {crop_width}x{crop_height} is smaller than the {width}x{height} \
             output the decoder reported"
        )));
    }
    let wide = frame.bit_depth() > 8;
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let dims = [
        (crop_x, crop_y, width as usize, height as usize),
        (crop_x / 2, crop_y / 2, cw as usize, ch as usize),
        (crop_x / 2, crop_y / 2, cw as usize, ch as usize),
    ];
    let mut planes: [Vec<u8>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for (index, &(origin_x, origin_y, plane_width, plane_height)) in dims.iter().enumerate() {
        let plane = &source.planes[index];
        let mut bytes = Vec::with_capacity(plane_width * plane_height * if wide { 2 } else { 1 });
        for row in 0..plane_height {
            let y = origin_y + row;
            let start = y * plane.stride + origin_x;
            let end = start + plane_width;
            if end > plane.data.len() || y >= plane.height {
                return Err(Error::backend(format!(
                    "the hevc {index} plane is {}x{} at stride {} but the crop needs rows up to \
                     {y} and columns up to {end}",
                    plane.width,
                    plane.height,
                    plane.stride
                )));
            }
            if wide {
                for sample in &plane.data[start..end] {
                    bytes.extend_from_slice(&sample.to_le_bytes());
                }
            } else {
                bytes.extend(plane.data[start..end].iter().map(|&sample| sample as u8));
            }
        }
        planes[index] = bytes;
    }
    let [y, u, v] = planes;
    if wide {
        p010_frame(width, height, y, u, v, pts)
    } else {
        i420_frame(width, height, y, u, v, pts)
    }
}

/// Registers the pure-Rust HEVC decoder.
///
/// HEVC is decode-only here: `rusty_h265` has no encoder.
pub fn register(registry: &mut Registry) {
    registry.register_decoder(
        CODEC,
        BackendKind::Rust,
        PRIORITY_BUILTIN,
        |config: &DecoderConfig| Ok(Some(Box::new(H265Decoder::new(config)?))),
        probe(CODEC, DECODER_CRATE, Direction::Decode),
    );
}

/// Says why this module cannot serve `codec` in `direction`, or `None` when it can.
pub fn unavailable(codec: Codec, direction: Direction) -> Option<BackendAvailability> {
    if codec == CODEC && direction == Direction::Decode {
        None
    } else if codec == CODEC {
        Some(BackendAvailability::Unavailable(format!(
            "the {DECODER_CRATE} crate has no {} for {codec}",
            Direction::Encode.name()
        )))
    } else {
        Some(BackendAvailability::Unavailable(format!(
            "the {DECODER_CRATE} tier serves h265 decode only, not {codec} ({})",
            direction.name()
        )))
    }
}

/// A structurally valid `hvcC` record carrying one one-byte NAL unit: enough for
/// the configuration parser, not enough for the codec. Used by the tests.
#[cfg(test)]
fn sample_hvcc() -> Vec<u8> {
    let mut record = vec![0u8; 23];
    record[0] = 1;
    record[22] = 1;
    record.push(0x80 | 32);
    record.extend_from_slice(&[0, 1]);
    record.extend_from_slice(&[0, 2, 0x40, 0x01]);
    record
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::buffer::Buffer;

    fn decoder_config() -> DecoderConfig {
        DecoderConfig::new(CODEC).with_size(16, 16)
    }

    #[test]
    fn the_decoder_rejects_any_other_codec() {
        for codec in Codec::ALL {
            let config = DecoderConfig::new(codec);
            let result = H265Decoder::new(&config);
            if codec == CODEC {
                assert!(result.is_ok(), "{codec} must be accepted");
            } else {
                let error = result.err().expect("only h265 may be accepted");
                assert_eq!(error.kind(), crate::error::ErrorKind::Unsupported);
                assert!(error.message().contains("h265"), "{}", error.message());
            }
        }
    }

    #[test]
    fn a_packet_without_start_codes_is_rejected_with_a_readable_reason() {
        let mut decoder = H265Decoder::new(&decoder_config()).unwrap();
        let error = decoder
            .push(Packet::new(CODEC, Buffer::from_slice(&[0x40, 0x01, 0x0c]), 0, 0))
            .unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("start code"), "{}", error.message());
    }

    #[test]
    fn unusable_extradata_is_rejected_with_a_readable_reason() {
        let config = decoder_config().with_extradata(Buffer::from_slice(&[1, 2, 3, 4]));
        let error = H265Decoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("hvcC"), "{}", error.message());
    }

    #[test]
    fn an_hvcc_whose_parameter_sets_do_not_parse_fails_loudly() {
        let config = decoder_config().with_extradata(Buffer::from_vec(sample_hvcc()));
        let error = H265Decoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("parameter set"), "{}", error.message());
    }

    #[test]
    fn a_header_packet_is_fed_as_parameter_sets() {
        let mut decoder = H265Decoder::new(&decoder_config()).unwrap();
        let error = decoder
            .push(Packet::header(CODEC, Buffer::from_vec(sample_hvcc())))
            .unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("parameter set"), "{}", error.message());
    }

    #[test]
    fn an_annex_b_parameter_set_blob_is_accepted_as_configuration() {
        // A one-byte VPS: structurally a NAL unit, semantically empty, so the
        // *decode* is rejected — this checks the shape is recognised at all.
        let blob = [0u8, 0, 0, 1, 0x40, 0x01];
        let config = decoder_config().with_extradata(Buffer::from_slice(&blob));
        let error = H265Decoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(!error.message().contains("hvcC"), "{}", error.message());
    }

    #[test]
    fn a_truncated_slice_is_an_error_not_a_panic() {
        let mut decoder = H265Decoder::new(&decoder_config()).unwrap();
        // An IDR_W_RADL NAL header with a one-byte payload.
        let packet = [0u8, 0, 0, 1, 0x26, 0x01];
        let error = decoder.push(Packet::new(CODEC, Buffer::from_slice(&packet), 7, 0)).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::Backend);
        assert!(decoder.receive().unwrap().is_none());
        assert!(decoder.flush().unwrap().is_empty());
    }

    #[test]
    fn eos_is_accepted_and_flushes_nothing_on_an_empty_stream() {
        let mut decoder = H265Decoder::new(&decoder_config()).unwrap();
        decoder.push(Packet::eos(CODEC)).unwrap();
        assert!(decoder.receive().unwrap().is_none());
        assert!(decoder.flush().unwrap().is_empty());
    }

    #[test]
    fn the_annex_b_hint_is_recorded() {
        let config = decoder_config().with_annex_b(true);
        assert!(H265Decoder::new(&config).unwrap().is_annex_b());
    }

    #[test]
    fn the_path_names_the_crate_and_the_software_tier() {
        let decoder = H265Decoder::new(&decoder_config()).unwrap();
        let path = decoder.path();
        assert_eq!(path.backend, BackendKind::Rust);
        assert!(path.is_software());
        assert!(!path.hardware_accelerated);
        assert!(path.detail.contains(DECODER_CRATE), "{}", path.detail);
        assert_eq!(decoder.codec(), CODEC);
    }

    #[test]
    fn unavailable_names_the_direction_it_does_not_serve() {
        assert!(unavailable(CODEC, Direction::Decode).is_none());
        let note = unavailable(CODEC, Direction::Encode).expect("h265 encode is not served");
        assert!(!note.is_available());
        assert!(note.reason().unwrap().contains("encoder"));
        let note = unavailable(Codec::Video(VideoCodec::H264), Direction::Decode)
            .expect("h264 is not this module's codec");
        assert!(!note.is_available());
        assert!(note.reason().unwrap().contains("h264"));
    }
}
