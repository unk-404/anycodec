import 'dart:typed_data';

/// The sample encodings this package can convert between.
///
/// The values deliberately mirror `anycodec`'s `SampleFormat` so a caller can
/// bridge an `AudioFrame` across with a one-line switch, but this package does
/// **not** depend on `anycodec`: it is pure computation and must stay usable on
/// the web, where the native engine does not exist.
enum PcmSampleFormat {
  /// Unsigned 8-bit, biased by 128 (silence is `128`).
  u8(1, 128),

  /// Signed 16-bit little-endian, the most common decoder output.
  s16(2, 0),

  /// Signed 32-bit little-endian.
  s32(4, 0),

  /// 32-bit IEEE-754, nominally in `[-1.0, 1.0]`.
  f32(4, 0);

  const PcmSampleFormat(this.bytesPerSample, this.silence);

  /// Bytes per single sample of a single channel.
  final int bytesPerSample;

  /// The raw byte value that means "silence".
  final int silence;
}

/// How a block of interleaved PCM is laid out.
///
/// Interleaved is the only layout this package accepts: every frame is
/// `channels` samples back to back, and frames are contiguous. Decoders that
/// produce planar output must interleave first, because a player's device
/// callback almost always wants interleaved.
final class PcmFormat {
  /// Creates a format.
  ///
  /// Throws [ArgumentError] for a non-positive sample rate or channel count: a
  /// silent zero here would otherwise turn into a division by zero deep inside a
  /// ring buffer, far from the mistake.
  PcmFormat({
    required this.sampleFormat,
    required this.sampleRate,
    required this.channels,
  }) {
    if (sampleRate <= 0) {
      throw ArgumentError.value(sampleRate, 'sampleRate', 'must be positive');
    }
    if (channels <= 0) {
      throw ArgumentError.value(channels, 'channels', 'must be positive');
    }
  }

  /// How each sample is encoded.
  final PcmSampleFormat sampleFormat;

  /// Frames (samples per channel) per second.
  final int sampleRate;

  /// Number of interleaved channels.
  final int channels;

  /// Bytes occupied by one frame across all channels.
  int get bytesPerFrame => sampleFormat.bytesPerSample * channels;

  /// Bytes occupied by [frames] frames.
  int bytesForFrames(int frames) => frames * bytesPerFrame;

  /// A format that differs only in the fields given.
  PcmFormat copyWith({
    PcmSampleFormat? sampleFormat,
    int? sampleRate,
    int? channels,
  }) => PcmFormat(
    sampleFormat: sampleFormat ?? this.sampleFormat,
    sampleRate: sampleRate ?? this.sampleRate,
    channels: channels ?? this.channels,
  );

  @override
  String toString() =>
      'PcmFormat(${sampleFormat.name} ${sampleRate}Hz x$channels)';

  @override
  bool operator ==(Object other) =>
      other is PcmFormat &&
      other.sampleFormat == sampleFormat &&
      other.sampleRate == sampleRate &&
      other.channels == channels;

  @override
  int get hashCode => Object.hash(sampleFormat, sampleRate, channels);
}

/// Converts interleaved [bytes] in [format] into interleaved `float32`.
///
/// This is the one conversion the rest of the package relies on: everything
/// downstream (the ring buffer, the FFT, the spectrum) works in normalised
/// `float32`, so the awkward part — unsigned bias, integer scaling, endianness —
/// happens exactly once, here.
///
/// Integer formats are scaled to `[-1.0, 1.0)`. `u8` is un-biased first
/// (`(value - 128) / 128`). Trailing bytes that do not form a whole sample are
/// ignored rather than read out of bounds.
///
/// Throws [ArgumentError] when `bytes` does not contain a whole number of frames.
Float32List pcmToFloat32(Uint8List bytes, PcmFormat format) {
  final bytesPerFrame = format.bytesPerFrame;
  if (bytesPerFrame == 0 || bytes.length % bytesPerFrame != 0) {
    throw ArgumentError.value(
      bytes.length,
      'bytes',
      'is not a whole number of ${format.bytesPerFrame}-byte frames '
          '(a partial frame cannot be converted without shifting the channels)',
    );
  }
  final samples = bytes.length ~/ format.sampleFormat.bytesPerSample;
  final out = Float32List(samples);
  final view = ByteData.sublistView(bytes);
  switch (format.sampleFormat) {
    case PcmSampleFormat.u8:
      for (var i = 0; i < samples; i++) {
        out[i] = (bytes[i] - 128) / 128.0;
      }
    case PcmSampleFormat.s16:
      for (var i = 0; i < samples; i++) {
        out[i] = view.getInt16(i * 2, Endian.little) / 32768.0;
      }
    case PcmSampleFormat.s32:
      for (var i = 0; i < samples; i++) {
        out[i] = view.getInt32(i * 4, Endian.little) / 2147483648.0;
      }
    case PcmSampleFormat.f32:
      for (var i = 0; i < samples; i++) {
        out[i] = view.getFloat32(i * 4, Endian.little);
      }
  }
  return out;
}

/// Converts interleaved normalised `float32` back into [format].
///
/// Integer targets are clamped: a float outside `[-1.0, 1.0]` would otherwise
/// wrap around and turn a loud passage into loud noise of the opposite sign.
Uint8List float32ToPcm(Float32List samples, PcmFormat format) {
  final out = Uint8List(samples.length * format.sampleFormat.bytesPerSample);
  final view = ByteData.sublistView(out);
  switch (format.sampleFormat) {
    case PcmSampleFormat.u8:
      for (var i = 0; i < samples.length; i++) {
        out[i] = (samples[i].clamp(-1.0, 1.0) * 127.0).round() + 128;
      }
    case PcmSampleFormat.s16:
      for (var i = 0; i < samples.length; i++) {
        view.setInt16(
          i * 2,
          (samples[i].clamp(-1.0, 1.0) * 32767.0).round(),
          Endian.little,
        );
      }
    case PcmSampleFormat.s32:
      for (var i = 0; i < samples.length; i++) {
        view.setInt32(
          i * 4,
          (samples[i].clamp(-1.0, 1.0) * 2147483647.0).round(),
          Endian.little,
        );
      }
    case PcmSampleFormat.f32:
      for (var i = 0; i < samples.length; i++) {
        view.setFloat32(i * 4, samples[i], Endian.little);
      }
  }
  return out;
}

/// Mixes interleaved [samples] with [channels] channels down to mono.
///
/// Averaging rather than summing: summing would clip a stereo signal that is
/// already near full scale, and a spectrum that clips is a spectrum of clipping
/// artefacts.
Float32List mixToMono(Float32List samples, int channels) {
  if (channels == 1) {
    return samples;
  }
  final frames = samples.length ~/ channels;
  final out = Float32List(frames);
  final scale = 1.0 / channels;
  for (var frame = 0; frame < frames; frame++) {
    var sum = 0.0;
    final base = frame * channels;
    for (var channel = 0; channel < channels; channel++) {
      sum += samples[base + channel];
    }
    out[frame] = sum * scale;
  }
  return out;
}
