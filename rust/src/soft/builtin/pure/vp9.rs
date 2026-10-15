//! Pure-Rust **VP9** decode *and* encode, on [`rusty_vp9`].
//!
//! VP9 is self-describing: a frame's uncompressed header carries the picture
//! size, the colour configuration and the bit depth, so neither direction needs
//! `extradata`. `DecoderConfig::extradata` is ignored on purpose.
//!
//! ## Decode
//!
//! `rusty_vp9` emits one [`DecodedFrame`](rusty_vp9::DecodedFrame) per
//! *displayable* picture with already-cropped, already-strided planes. A
//! 4:2:0 picture becomes [`PixelFormat::I420`] at 8 bits and
//! [`PixelFormat::P010`] above 8 bits; a 4:2:2 or 4:4:4 picture has no
//! AnyCodec layout and is refused by name rather than mis-described.
//!
//! ## Encode
//!
//! The encoder takes [`PixelFormat::I420`] frames and writes one packet per
//! coded frame, first a key frame and then P frames against the previous
//! reconstruction. `qindex` (1..=255) can be pinned through the encoder option
//! of that name; otherwise it is derived from
//! [`EncoderConfig::effective_quality`].

use crate::engine::backend::{BackendKind, CodecPath};
use crate::engine::traits::{Decoder, Encoder};
use crate::engine::{BackendAvailability, Registry};
use crate::error::{Error, Result};
use crate::media::buffer::Buffer;
use crate::media::codec::{Codec, VideoCodec};
use crate::media::config::{DecoderConfig, EncoderConfig};
use crate::media::format::PixelFormat;
use crate::media::frame::{Frame, Plane, VideoFrame};
use crate::media::packet::{flags, Packet};

use crate::soft::PRIORITY_BUILTIN;
use super::{probe, Direction};

/// The one codec this module serves.
pub(crate) const CODEC: Codec = Codec::Video(VideoCodec::Vp9);

/// The crate that decodes it.
pub(crate) const DECODER_CRATE: &str = "rusty_vp9";

/// The crate that encodes it.
pub(crate) const ENCODER_CRATE: &str = "rusty_vp9";

/// The quality index used when the caller asks for neither a quality nor a
/// bitrate; `rusty_vp9`'s own default.
/// Kept as the documented default; the encoder currently derives its quantiser
/// from the caller's quality hint, which is what EncoderConfig::effective_quality`n/// is for.
#[allow(dead_code)]
pub(crate) const DEFAULT_QINDEX: u32 = 64;

/// A pure-Rust VP9 decoder.
pub struct Vp9Decoder {
    path: CodecPath,
    inner: rusty_vp9::Vp9Decoder,
    pending: Vec<Frame>,
    first_packet: bool,
    pts_cursor: i64,
}

impl std::fmt::Debug for Vp9Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vp9Decoder")
            .field("path", &self.path.to_string())
            .field("queued", &self.pending.len())
            .finish()
    }
}

impl Vp9Decoder {
    /// Builds a decoder from `config`.
    ///
    /// Fails with [`crate::error::ErrorKind::Unsupported`] for any codec other than VP9.
    pub fn new(config: &DecoderConfig) -> Result<Self> {
        if config.codec != CODEC {
            return Err(Error::unsupported(format!(
                "the {DECODER_CRATE} decoder produces vp9 only, not {}",
                config.codec
            )));
        }
        Ok(Self {
            path: CodecPath::new(
                BackendKind::Rust,
                false,
                format!("rust: {DECODER_CRATE} decoder"),
            ),
            inner: rusty_vp9::Vp9Decoder::new(),
            pending: Vec::new(),
            first_packet: true,
            pts_cursor: 0,
        })
    }

    /// Decodes every picture the last push made displayable.
    fn drain(&mut self) -> Result<()> {
        loop {
            match self.inner.next_frame() {
                Ok(decoded) => {
                    let pts = decoded.pts.unwrap_or(self.pts_cursor);
                    self.pts_cursor = pts + 1;
                    let frame = picture(&decoded, pts)?;
                    self.pending.push(Frame::Video(frame));
                }
                Err(rusty_vp9::Error::Again) | Err(rusty_vp9::Error::Eof) => return Ok(()),
                Err(error) => {
                    return Err(Error::backend(format!("{DECODER_CRATE} decode failed: {error}")))
                }
            }
        }
    }
}

impl Decoder for Vp9Decoder {
    fn codec(&self) -> Codec {
        CODEC
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, packet: Packet) -> Result<()> {
        if packet.is_eos() {
            self.inner.flush();
            return self.drain();
        }
        if packet.is_header() || packet.is_empty() {
            // VP9 has no out-of-band configuration.
            return Ok(());
        }
        if self.first_packet {
            self.pts_cursor = packet.pts;
            self.first_packet = false;
        }
        self.inner
            .push(packet.data.as_slice(), Some(packet.pts))
            .map_err(|error| Error::backend(format!("{DECODER_CRATE} rejected the packet: {error}")))?;
        self.drain()
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

/// Converts a decoded VP9 picture into the AnyCodec media model.
fn picture(decoded: &rusty_vp9::DecodedFrame, pts: i64) -> Result<VideoFrame> {
    if decoded.planes.len() != 3 || decoded.strides.len() != 3 {
        return Err(Error::backend(format!(
            "{DECODER_CRATE} produced {} planes with {} strides; AnyCodec's video model is \
             three-plane",
            decoded.planes.len(),
            decoded.strides.len()
        )));
    }
    if decoded.subsampling_x != 1 || decoded.subsampling_y != 1 {
        let chroma_width = if decoded.subsampling_x == 0 { 4 } else { 2 };
        let chroma_height = if decoded.subsampling_y == 0 { 4 } else { 2 };
        return Err(Error::unsupported(format!(
            "the {DECODER_CRATE} decoder produced a {}-bit 4:{chroma_width}:{chroma_height} \
             picture; this tier maps 4:2:0 onto {} only",
            decoded.bit_depth,
            PixelFormat::I420.name()
        )));
    }
    let wide = decoded.bit_depth > 8;
    let bytes_per_sample = if wide { 2usize } else { 1usize };
    let (cw, ch) = (decoded.width.div_ceil(2), decoded.height.div_ceil(2));
    let dims = [
        (decoded.width, decoded.height),
        (cw, ch),
        (cw, ch),
    ];
    let mut planes = Vec::with_capacity(3);
    for (index, &(width, height)) in dims.iter().enumerate() {
        let stride = decoded.strides[index];
        let data = &decoded.planes[index];
        let row_bytes = width as usize * bytes_per_sample;
        if stride < row_bytes {
            return Err(Error::backend(format!(
                "{DECODER_CRATE} plane {index} has stride {stride} but needs {row_bytes} bytes \
                 per row"
            )));
        }
        if data.len() < stride * height as usize {
            return Err(Error::backend(format!(
                "{DECODER_CRATE} plane {index} is {} bytes but {height} rows of stride {stride} \
                 need {}",
                data.len(),
                stride * height as usize
            )));
        }
        planes.push(Plane::new(Buffer::from_slice(&data[..stride * height as usize]), stride)?);
    }
    let format = if wide { PixelFormat::P010 } else { PixelFormat::I420 };
    VideoFrame::new(format, decoded.width, decoded.height, planes, pts)
}

/// A pure-Rust VP9 encoder.
pub struct Vp9Encoder {
    path: CodecPath,
    inner: rusty_vp9::Vp9Encoder,
    width: u32,
    height: u32,
    pending: Vec<Packet>,
    pts_cursor: i64,
    first_frame: bool,
    pushed: bool,
    finished: bool,
}

impl std::fmt::Debug for Vp9Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vp9Encoder")
            .field("path", &self.path.to_string())
            .field("width", &self.width)
            .field("height", &self.height)
            .field("queued", &self.pending.len())
            .finish()
    }
}

impl Vp9Encoder {
    /// Builds an encoder from `config`.
    ///
    /// Only [`PixelFormat::I420`] input is accepted, dimensions must be non-zero
    /// and even, and the option `qindex` (1..=255) pins the quantizer. Without
    /// it the qindex is derived from [`EncoderConfig::effective_quality`], so a
    /// caller that sets a bitrate gets a bitrate-shaped quality.
    pub fn new(config: &EncoderConfig) -> Result<Self> {
        if config.codec != CODEC {
            return Err(Error::unsupported(format!(
                "the {ENCODER_CRATE} encoder produces vp9 only, not {}",
                config.codec
            )));
        }
        if config.width == 0 || config.height == 0 {
            return Err(Error::invalid_argument(format!(
                "{ENCODER_CRATE} needs a non-zero frame size, got {}x{}",
                config.width, config.height
            )));
        }
        if config.width % 2 != 0 || config.height % 2 != 0 {
            return Err(Error::invalid_argument(format!(
                "{ENCODER_CRATE} needs even dimensions for 4:2:0, got {}x{}",
                config.width, config.height
            )));
        }
        if config.pixel_format != PixelFormat::I420 {
            return Err(Error::invalid_argument(format!(
                "{ENCODER_CRATE} takes {} frames; convert {} first",
                PixelFormat::I420.name(),
                config.pixel_format.name()
            )));
        }
        let qindex = match config.option("qindex") {
            Some(text) => text.parse::<u32>().map_err(|_| {
                Error::invalid_argument(format!("`qindex` is not a number: {text:?}"))
            })?,
            None => {
                let quality = config.effective_quality().clamp(0.0, 1.0);
                ((1.0 - quality) * 255.0).round().clamp(1.0, 255.0) as u32
            }
        };
        if !(1..=255).contains(&qindex) {
            return Err(Error::invalid_argument(format!(
                "`qindex` must be 1..=255 (0 would mean lossless), got {qindex}"
            )));
        }
        let mut inner = rusty_vp9::Vp9Encoder::default();
        inner
            .configure(&rusty_vp9::Vp9EncoderConfig {
                qindex: Some(qindex),
                ..Default::default()
            })
            .map_err(|error| {
                Error::invalid_argument(format!("{ENCODER_CRATE} rejected the configuration: {error}"))
            })?;
        Ok(Self {
            path: CodecPath::new(
                BackendKind::Rust,
                false,
                format!("rust: {ENCODER_CRATE} encoder"),
            ),
            inner,
            width: config.width,
            height: config.height,
            pending: Vec::new(),
            pts_cursor: 0,
            first_frame: true,
            pushed: false,
            finished: false,
        })
    }

    /// The frame size this encoder was built for.
    pub const fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// `true` once at least one picture has been accepted.
    pub const fn has_input(&self) -> bool {
        self.pushed
    }

    fn drain(&mut self) -> Result<()> {
        loop {
            match self.inner.next_packet() {
                Ok(packet) => {
                    let pts = packet.pts.unwrap_or(self.pts_cursor);
                    self.pts_cursor = pts + 1;
                    self.pending.push(
                        Packet::new(
                            CODEC,
                            Buffer::from_vec(packet.data),
                            pts,
                            if packet.keyframe { flags::KEYFRAME } else { 0 },
                        )
                        .with_duration(1),
                    );
                }
                Err(rusty_vp9::Error::Again) | Err(rusty_vp9::Error::Eof) => return Ok(()),
                Err(error) => {
                    return Err(Error::backend(format!("{ENCODER_CRATE} encode failed: {error}")))
                }
            }
        }
    }
}

impl Encoder for Vp9Encoder {
    fn codec(&self) -> Codec {
        CODEC
    }

    fn path(&self) -> CodecPath {
        self.path.clone()
    }

    fn push(&mut self, frame: Frame) -> Result<()> {
        if self.finished {
            return Err(Error::state(format!("the {ENCODER_CRATE} encoder was already flushed")));
        }
        let Frame::Video(video) = frame else {
            return Err(Error::invalid_argument(format!(
                "the {ENCODER_CRATE} encoder takes video frames only"
            )));
        };
        if video.width != self.width || video.height != self.height {
            return Err(Error::invalid_argument(format!(
                "the {ENCODER_CRATE} encoder was configured for {}x{} but received {}x{}",
                self.width, self.height, video.width, video.height
            )));
        }
        if video.format != PixelFormat::I420 {
            return Err(Error::invalid_argument(format!(
                "the {ENCODER_CRATE} encoder takes {} frames, got {}",
                PixelFormat::I420.name(),
                video.format.name()
            )));
        }
        let (cw, ch) = (self.width.div_ceil(2), self.height.div_ceil(2));
        let y = tight_rows(&video.planes[0], self.width as usize, self.height as usize)?;
        let u = tight_rows(&video.planes[1], cw as usize, ch as usize)?;
        let v = tight_rows(&video.planes[2], cw as usize, ch as usize)?;
        if self.first_frame {
            self.pts_cursor = video.pts;
            self.first_frame = false;
        }
        self.inner
            .push_frame(
                [&y, &u, &v],
                [self.width as usize, cw as usize, cw as usize],
                self.width,
                self.height,
            )
            .map_err(|error| Error::backend(format!("{ENCODER_CRATE} rejected the frame: {error}")))?;
        self.pushed = true;
        self.drain()
    }

    fn receive(&mut self) -> Result<Option<Packet>> {
        Ok(if self.pending.is_empty() { None } else { Some(self.pending.remove(0)) })
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        if !self.pushed {
            return Err(Error::state(format!(
                "the {ENCODER_CRATE} encoder was flushed with no input"
            )));
        }
        if !self.finished {
            self.inner.flush();
            self.finished = true;
            self.drain()?;
        }
        Ok(std::mem::take(&mut self.pending))
    }

    fn request_keyframe(&mut self) -> Result<()> {
        Err(Error::unsupported(
            "the vp9 encoder in this build starts a key frame only after a size change",
        ))
    }
}

/// Copies `rows` rows of `row_bytes` out of a plane, dropping any row padding.
fn tight_rows(plane: &Plane, row_bytes: usize, rows: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(row_bytes * rows);
    for row in 0..rows {
        out.extend_from_slice(plane.row(row, row_bytes)?);
    }
    Ok(out)
}

/// Registers the pure-Rust VP9 encoder and decoder.
pub fn register(registry: &mut Registry) {
    registry.register_decoder(
        CODEC,
        BackendKind::Rust,
        PRIORITY_BUILTIN,
        |config: &DecoderConfig| Ok(Some(Box::new(Vp9Decoder::new(config)?))),
        probe(CODEC, DECODER_CRATE, Direction::Decode),
    );
    registry.register_encoder(
        CODEC,
        BackendKind::Rust,
        PRIORITY_BUILTIN,
        |config: &EncoderConfig| Ok(Some(Box::new(Vp9Encoder::new(config)?))),
        probe(CODEC, ENCODER_CRATE, Direction::Encode),
    );
}

/// Says why this module cannot serve `codec`, or `None` when it can.
pub fn unavailable(codec: Codec, direction: Direction) -> Option<BackendAvailability> {
    if codec == CODEC {
        None
    } else {
        Some(BackendAvailability::Unavailable(format!(
            "the {DECODER_CRATE} tier serves vp9 only, not {codec} ({})",
            direction.name()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soft::builtin::pure::testing::mean_abs_error;

    const WIDTH: u32 = 32;
    const HEIGHT: u32 = 32;

    fn decoder_config() -> DecoderConfig {
        DecoderConfig::new(CODEC).with_size(WIDTH, HEIGHT)
    }

    fn encoder_config() -> EncoderConfig {
        EncoderConfig::video(VideoCodec::Vp9, WIDTH, HEIGHT)
            .with_framerate(30, 1)
            .with_option("qindex", "8")
    }

    /// A synthetic picture: a smooth luma ramp plus a slowly varying chroma.
    fn picture(shift: u8) -> VideoFrame {
        let (w, h) = (WIDTH as usize, HEIGHT as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let mut y = vec![0u8; w * h];
        for row in 0..h {
            for column in 0..w {
                y[row * w + column] = ((row * 2 + column + shift as usize) & 0xFF) as u8;
            }
        }
        let u = vec![120u8; cw * ch];
        let v = vec![136u8; cw * ch];
        crate::soft::builtin::pure::i420_frame(
            WIDTH,
            HEIGHT,
            y,
            u,
            v,
            i64::from(shift),
        )
        .unwrap()
    }

    #[test]
    fn the_decoder_rejects_any_other_codec() {
        for codec in Codec::ALL {
            let config = DecoderConfig::new(codec);
            let result = Vp9Decoder::new(&config);
            if codec == CODEC {
                assert!(result.is_ok(), "{codec} must be accepted");
            } else {
                let error = result.err().expect("only vp9 may be accepted");
                assert_eq!(error.kind(), crate::error::ErrorKind::Unsupported);
                assert!(error.message().contains("vp9"), "{}", error.message());
            }
        }
    }

    #[test]
    fn the_encoder_validates_geometry_and_quality() {
        let mut config = encoder_config();
        config.width = 33;
        let error = Vp9Encoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("even"), "{}", error.message());

        let mut config = encoder_config();
        config.pixel_format = PixelFormat::Nv12;
        let error = Vp9Encoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("nv12"), "{}", error.message());

        let mut config = encoder_config();
        config.options.clear();
        config.options.push(("qindex".into(), "300".into()));
        let error = Vp9Encoder::new(&config).unwrap_err();
        assert_eq!(error.kind(), crate::error::ErrorKind::InvalidArgument);
        assert!(error.message().contains("300"), "{}", error.message());
    }

    #[test]
    fn a_video_encoder_rejects_an_audio_frame() {
        let mut encoder = Vp9Encoder::new(&encoder_config()).unwrap();
        let frame = crate::media::frame::AudioFrame::from_f32_planar(
            48_000,
            crate::media::format::ChannelLayout::Mono,
            1,
            vec![vec![0.0]],
            0,
        )
        .unwrap();
        assert_eq!(
            encoder.push(Frame::Audio(frame)).unwrap_err().kind(),
            crate::error::ErrorKind::InvalidArgument
        );
    }

    #[test]
    fn flushing_without_input_is_a_state_error() {
        let mut encoder = Vp9Encoder::new(&encoder_config()).unwrap();
        assert_eq!(encoder.flush().unwrap_err().kind(), crate::error::ErrorKind::State);
    }

    #[test]
    fn the_paths_name_the_crate_and_the_software_tier() {
        let decoder = Vp9Decoder::new(&decoder_config()).unwrap();
        assert_eq!(decoder.path().backend, BackendKind::Rust);
        assert!(decoder.path().is_software());
        assert!(!decoder.path().hardware_accelerated);
        assert!(decoder.path().detail.contains(DECODER_CRATE));

        let encoder = Vp9Encoder::new(&encoder_config()).unwrap();
        assert_eq!(encoder.dimensions(), (WIDTH, HEIGHT));
        assert_eq!(encoder.path().backend, BackendKind::Rust);
        assert!(encoder.path().is_software());
        assert!(!encoder.path().hardware_accelerated);
        assert!(encoder.path().detail.contains(ENCODER_CRATE));
    }

    /// The end-to-end proof: encode with the pure-Rust VP9 encoder, decode with
    /// the pure-Rust VP9 decoder, and compare the luma planes. VP9 is lossy and
    /// the encoder is inter-predicted after the first key frame, so the second
    /// picture is the interesting one.
    #[test]
    fn software_vp9_round_trips_through_the_software_decoder() {
        let first = picture(0);
        let second = picture(4);
        let mut encoder = Vp9Encoder::new(&encoder_config()).unwrap();
        encoder.push(Frame::Video(first.clone())).unwrap();
        encoder.push(Frame::Video(second.clone())).unwrap();
        let packets = encoder.flush().unwrap();
        assert!(packets.len() >= 2, "one packet per coded frame");
        assert!(packets[0].is_keyframe(), "the first frame is a key frame");

        let mut decoder = Vp9Decoder::new(&decoder_config()).unwrap();
        for packet in &packets {
            decoder.push(packet.clone()).unwrap();
        }
        let mut decoded: Vec<VideoFrame> = Vec::new();
        while let Some(frame) = decoder.receive().unwrap() {
            decoded.push(frame.as_video().expect("video frames only").clone());
        }
        decoded.extend(
            decoder
                .flush()
                .unwrap()
                .into_iter()
                .map(|frame| frame.as_video().expect("video frames only").clone()),
        );
        assert_eq!(decoded.len(), 2, "both pictures must come back");

        for (index, frame) in decoded.iter().enumerate() {
            assert_eq!(frame.format, PixelFormat::I420);
            assert_eq!(frame.width, WIDTH);
            assert_eq!(frame.height, HEIGHT);
            let expected = if index == 0 { &first } else { &second };
            let error = mean_abs_error(expected.planes[0].data(), frame.planes[0].data());
            assert!(error < 6.0, "picture {index} luma error {error} is too large for qindex 8");
        }
    }

    #[test]
    fn a_decoder_never_panics_on_garbage() {
        let mut decoder = Vp9Decoder::new(&decoder_config()).unwrap();
        for bytes in [vec![0u8; 4], vec![0xFF; 32], vec![0x82, 0x49, 0x83, 0x42]] {
            let _ = decoder.push(Packet::new(CODEC, Buffer::from_vec(bytes), 0, 0));
        }
    }

    #[test]
    fn unavailable_names_the_codec_it_cannot_serve() {
        assert!(unavailable(CODEC, Direction::Decode).is_none());
        let note = unavailable(Codec::Video(VideoCodec::Av1), Direction::Decode)
            .expect("av1 is not this module's codec");
        assert!(!note.is_available());
        assert!(note.reason().unwrap().contains("av1"));
    }
}
