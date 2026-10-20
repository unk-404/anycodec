/// Pixel formats, sample formats and channel layouts.
///
/// These mirror `rust/src/media/format.rs` exactly, including the plane geometry
/// arithmetic. That arithmetic matters: the C ABI takes *raw pointers* for a
/// video frame's planes, and the Rust side reads `stride * plane_height` bytes
/// from each one. [PixelFormat.planeHeight] / [PixelFormat.effectiveStride] are
/// what the Dart layer uses to check a frame is big enough *before* handing the
/// pointers to native code, so a short buffer becomes an
/// `AnyCodecException` rather than an out-of-bounds read.
library;

/// How the samples of a raw picture are arranged.
///
/// The `raw` values are the `ANYCODEC_PIXEL_FORMAT_*` constants of the C ABI.
enum PixelFormat {
  /// 3 planes: full-rate luma, half-rate chroma, half-rate chroma.
  i420(0x1, 3, 8),

  /// 2 planes: full-rate luma, interleaved half-rate chroma (U then V).
  nv12(0x2, 2, 8),

  /// 2 planes: full-rate luma, interleaved half-rate chroma (V then U).
  nv21(0x3, 2, 8),

  /// 1 plane, 8 bits per channel, R first.
  rgba8(0x4, 1, 8),

  /// 1 plane, 8 bits per channel, B first.
  bgra8(0x5, 1, 8),

  /// 1 plane, 4:2:2 packed as Y0 U Y1 V.
  yuy2(0x6, 1, 8),

  /// 3 planes, 10-bit little-endian in 16-bit words.
  p010(0x7, 3, 10);

  const PixelFormat(this.raw, this.planeCount, this.bitDepth);

  /// The `ANYCODEC_PIXEL_FORMAT_*` value used by the C ABI.
  final int raw;

  /// Number of independently addressed planes this format carries.
  final int planeCount;

  /// Bits per luma sample; `10` only for [PixelFormat.p010].
  final int bitDepth;

  /// `true` when the format stores an alpha channel.
  bool get hasAlpha => this == PixelFormat.rgba8 || this == PixelFormat.bgra8;

  /// `true` for the two packed RGB formats.
  bool get isRgb => this == PixelFormat.rgba8 || this == PixelFormat.bgra8;

  /// The number of bytes one luma sample occupies.
  int get bytesPerSample => bitDepth > 8 ? 2 : 1;

  /// The format whose ABI identifier is [raw], or `null` when unknown.
  static PixelFormat? fromRaw(int raw) {
    for (final format in values) {
      if (format.raw == raw) {
        return format;
      }
    }
    return null;
  }

  /// Width in *samples* of plane [index] for a picture [width] pixels wide.
  ///
  /// Returns `null` when [index] is not a plane of this format. A width of `0`
  /// is not special-cased: the ABI never sees one for a valid picture.
  int? planeWidth(int width, int index) {
    if (index < 0 || index >= planeCount) {
      return null;
    }
    final chromaWidth = (width + 1) ~/ 2;
    return switch (this) {
      PixelFormat.i420 || PixelFormat.p010 => index == 0 ? width : chromaWidth,
      PixelFormat.nv12 ||
      PixelFormat.nv21 => index == 0 ? width : chromaWidth * 2,
      PixelFormat.yuy2 => index == 0 ? width * 2 : null,
      PixelFormat.rgba8 || PixelFormat.bgra8 => width * 4,
    };
  }

  /// Height in rows of plane [index] for a picture [height] pixels high.
  int? planeHeight(int height, int index) {
    if (index < 0 || index >= planeCount) {
      return null;
    }
    final chromaHeight = (height + 1) ~/ 2;
    return switch (this) {
      PixelFormat.i420 ||
      PixelFormat.p010 => index == 0 ? height : chromaHeight,
      PixelFormat.nv12 ||
      PixelFormat.nv21 => index == 0 ? height : chromaHeight,
      PixelFormat.yuy2 || PixelFormat.rgba8 || PixelFormat.bgra8 => height,
    };
  }

  /// The tightly packed row stride of plane [index], in bytes.
  ///
  /// Returns `null` when [index] is not a plane of this format.
  int? defaultStride(int width, int index) {
    final samples = planeWidth(width, index);
    return samples == null ? null : samples * bytesPerSample;
  }

  /// The stride the Rust side will actually use for plane [index].
  ///
  /// `rust/src/ffi/encoder.rs` takes `max(caller_stride, default_stride)`, so a
  /// caller that under-reports its stride still gets a correct (if slower)
  /// copy — but only if the plane buffer is big enough for the *larger* of the
  /// two. Use this value, times [planeHeight], as the minimum plane length.
  int? effectiveStride(int width, int index, int callerStride) {
    final packed = defaultStride(width, index);
    if (packed == null) {
      return null;
    }
    return callerStride > packed ? callerStride : packed;
  }

  /// The minimum number of bytes plane [index] must contain.
  int? minimumPlaneLength(int width, int height, int index, int callerStride) {
    final stride = effectiveStride(width, index, callerStride);
    final rows = planeHeight(height, index);
    return (stride == null || rows == null) ? null : stride * rows;
  }
}

/// How the samples of a raw audio frame are encoded.
enum SampleFormat {
  /// 32-bit IEEE-754 float, interleaved.
  f32(0x1, 4),

  /// Signed 16-bit little-endian, interleaved.
  s16(0x2, 2),

  /// Signed 32-bit little-endian, interleaved.
  s32(0x3, 4),

  /// Unsigned 8-bit, interleaved.
  u8(0x4, 1);

  const SampleFormat(this.raw, this.bytesPerSample);

  /// The `ANYCODEC_SAMPLE_FORMAT_*` value used by the C ABI.
  final int raw;

  /// Bytes used by one sample of one channel.
  final int bytesPerSample;

  /// The format whose ABI identifier is [raw], or `null` when unknown.
  static SampleFormat? fromRaw(int raw) {
    for (final format in values) {
      if (format.raw == raw) {
        return format;
      }
    }
    return null;
  }
}

/// Speaker arrangement of an audio frame.
enum ChannelLayout {
  /// One channel.
  mono(0x1, 1),

  /// Two channels, left then right.
  stereo(0x2, 2),

  /// Three channels: front-left, front-right, low frequency.
  stereo21(0x3, 3),

  /// Four channels: front-left, front-right, back-left, back-right.
  quad(0x4, 4),

  /// Six channels (5.1).
  fiveOne(0x5, 6),

  /// Eight channels (7.1).
  sevenOne(0x6, 8);

  const ChannelLayout(this.raw, this.channels);

  /// The `ANYCODEC_CHANNEL_LAYOUT_*` value used by the C ABI.
  final int raw;

  /// Number of channels.
  final int channels;

  /// The layout whose ABI identifier is [raw], or `null` when unknown.
  static ChannelLayout? fromRaw(int raw) {
    for (final layout in values) {
      if (layout.raw == raw) {
        return layout;
      }
    }
    return null;
  }

  /// Maps a plain channel count onto the layout the ABI uses for it.
  ///
  /// Returns `null` for `0`, `5`, `7` and anything above `8`: the ABI has no
  /// identifier for those, and silently picking a different layout would be a
  /// lie.
  static ChannelLayout? fromChannelCount(int count) => switch (count) {
    1 => ChannelLayout.mono,
    2 => ChannelLayout.stereo,
    3 => ChannelLayout.stereo21,
    4 => ChannelLayout.quad,
    6 => ChannelLayout.fiveOne,
    8 => ChannelLayout.sevenOne,
    _ => null,
  };

  /// Interleaved byte size of [frames] sample frames in [format].
  int byteLength(int frames, SampleFormat format) =>
      channels * frames * format.bytesPerSample;
}
