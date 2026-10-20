/// Codec identifiers shared by every layer of the Dart API.
///
/// The numeric values are the `ANYCODEC_CODEC_*` constants of the frozen C ABI
/// (`rust/include/anycodec.h`); [Codec.raw] is what actually goes over FFI.
///
/// The declaration order is load-bearing: it matches `Codec::ALL` on the Rust
/// side, which is the order `anycodec_capabilities` reports codecs in.
library;

/// Whether a codec compresses pictures or sound.
enum MediaKind {
  /// A picture codec (H.264, VP9, AV1, ...).
  video,

  /// A sound codec (AAC, Opus, FLAC, ...).
  audio,
}

/// Every codec AnyCodec routes.
///
/// [raw] is the C ABI identifier. Use [Codec.fromRaw] to go the other way; it
/// returns `null` for an identifier this build does not know, which is how the
/// Dart layer turns an unknown ABI value into a clear [ArgumentError] instead of
/// undefined behaviour.
enum Codec {
  /// H.264 / AVC (ITU-T H.264, ISO-IEC 14496-10).
  h264(MediaKind.video, 0x1, 'H.264/AVC'),

  /// H.265 / HEVC (ITU-T H.265, ISO-IEC 23008-2).
  h265(MediaKind.video, 0x2, 'H.265/HEVC'),

  /// H.266 / VVC (ITU-T H.266, ISO-IEC 23090-3).
  h266(MediaKind.video, 0x3, 'H.266/VVC'),

  /// Google VP8 (RFC 6386).
  vp8(MediaKind.video, 0x4, 'VP8'),

  /// Google VP9.
  vp9(MediaKind.video, 0x5, 'VP9'),

  /// AOMedia AV1.
  av1(MediaKind.video, 0x6, 'AV1'),

  /// MPEG-1/2 Audio Layer III.
  mp3(MediaKind.audio, 0x101, 'MP3'),

  /// MPEG-4 Advanced Audio Coding.
  aac(MediaKind.audio, 0x102, 'AAC'),

  /// Xiph Vorbis.
  vorbis(MediaKind.audio, 0x103, 'Vorbis'),

  /// IETF Opus (RFC 6716).
  opus(MediaKind.audio, 0x104, 'Opus'),

  /// Xiph FLAC (lossless).
  flac(MediaKind.audio, 0x105, 'FLAC'),

  /// Apple Lossless Audio Codec.
  alac(MediaKind.audio, 0x106, 'ALAC');

  const Codec(this.kind, this.raw, this.label);

  /// Whether this codec compresses pictures or sound.
  final MediaKind kind;

  /// The `ANYCODEC_CODEC_*` value used by the C ABI.
  final int raw;

  /// Human readable name, e.g. `"H.265/HEVC"`.
  final String label;

  /// `true` for picture codecs.
  bool get isVideo => kind == MediaKind.video;

  /// `true` for sound codecs.
  bool get isAudio => kind == MediaKind.audio;

  /// The codec whose ABI identifier is [raw], or `null` when unknown.
  ///
  /// Never throws: an unrecognised identifier is a caller bug, and callers get
  /// to decide whether to raise [ArgumentError] or ignore it.
  static Codec? fromRaw(int raw) {
    for (final codec in values) {
      if (codec.raw == raw) {
        return codec;
      }
    }
    return null;
  }

  /// The codec whose canonical name is [name] (`"h264"`, `"opus"`, ...), or
  /// `null` when unknown.
  ///
  /// Matching is case-insensitive and ignores `-`, `_`, `.` and spaces, so
  /// `"H.264"`, `"h-264"` and `"h264"` are all accepted.
  static Codec? fromName(String name) {
    final normalised = name.toLowerCase().replaceAll(RegExp(r'[-_. ]'), '');
    for (final codec in values) {
      if (codec.name == normalised) {
        return codec;
      }
    }
    return switch (normalised) {
      'avc' || 'avc1' || 'x264' => Codec.h264,
      'hevc' || 'hvc1' || 'hev1' || 'x265' => Codec.h265,
      'vvc' || 'vvc1' => Codec.h266,
      'av01' => Codec.av1,
      'mpeg3' || 'mpga' => Codec.mp3,
      'mp4a' || 'aaclc' => Codec.aac,
      _ => null,
    };
  }

  @override
  String toString() => '$name ($label)';
}

/// Whether a codec is being used to compress or to decompress.
///
/// The ABI calls this `kind`; this enum is the Dart spelling of it. `0` is
/// encode and `1` is decode, matching `AnyCodecCapability::kind`.
enum CodecDirection {
  /// Producing compressed packets from raw frames.
  encode(0, 'encoder'),

  /// Producing raw frames from compressed packets.
  decode(1, 'decoder');

  const CodecDirection(this.raw, this.token);

  /// The `0`/`1` value used by the C ABI.
  final int raw;

  /// `"encoder"` or `"decoder"`, matching the Rust side's own vocabulary.
  final String token;

  /// Parses an ABI direction value, or returns `null` when it is not `0`/`1`.
  static CodecDirection? fromRaw(int raw) => switch (raw) {
    0 => CodecDirection.encode,
    1 => CodecDirection.decode,
    _ => null,
  };

  /// The other direction.
  CodecDirection get opposite => this == CodecDirection.encode
      ? CodecDirection.decode
      : CodecDirection.encode;
}
