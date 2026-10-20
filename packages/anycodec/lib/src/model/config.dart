/// Encoder and decoder configuration, plus backend selection policy.
library;

import 'dart:typed_data';

import 'codec.dart';
import 'format.dart';

/// Which backends a caller is willing to accept.
///
/// The values are the `ANYCODEC_PREFERENCE_*` constants of the C ABI.
enum BackendPreference {
  /// Platform frameworks first, then software fallbacks.
  auto(0, 'auto'),

  /// Platform frameworks only: fail rather than silently fall back to software.
  nativeOnly(1, 'native_only'),

  /// Software codecs only: never touch a platform framework.
  softwareOnly(2, 'software_only'),

  /// Only implementations known to run on dedicated hardware.
  requireHardware(3, 'require_hardware');

  const BackendPreference(this.raw, this.token);

  /// The `ANYCODEC_PREFERENCE_*` value used by the C ABI.
  final int raw;

  /// The lower-case token the Rust side uses in its error messages.
  final String token;

  /// Parses an ABI preference value, or returns `null` when it is out of range.
  static BackendPreference? fromRaw(int raw) {
    for (final preference in values) {
      if (preference.raw == raw) {
        return preference;
      }
    }
    return null;
  }
}

/// A speed/size trade-off hint for an encoder.
enum Preset {
  /// Lowest latency.
  speed(0),

  /// Platform default.
  balanced(1),

  /// Smallest output.
  quality(2);

  const Preset(this.raw);

  /// The `ANYCODEC_PRESET_*` value used by the C ABI.
  final int raw;

  /// Parses an ABI preset value, or returns `null` when it is out of range.
  static Preset? fromRaw(int raw) {
    for (final preset in values) {
      if (preset.raw == raw) {
        return preset;
      }
    }
    return null;
  }
}

/// Everything needed to start an encoder.
///
/// ## Why every field is always set
///
/// `anycodec_encoder_new` validates *all* of its enum fields, even the ones the
/// selected codec ignores: a video encoder must still be given a valid
/// [sampleFormat] and [channels], and an audio encoder a valid [pixelFormat].
/// The named constructors therefore fill in legal defaults instead of leaving
/// the irrelevant fields at `0`, which the ABI would reject as
/// `invalid_argument`.
final class EncoderConfig {
  /// Builds a complete configuration.
  ///
  /// Prefer [EncoderConfig.video] and [EncoderConfig.audio]; this constructor
  /// exists so a configuration read back from a wire format can be rebuilt
  /// field for field.
  const EncoderConfig({
    required this.codec,
    this.width = 0,
    this.height = 0,
    this.pixelFormat = PixelFormat.i420,
    this.framerateNumerator = 30,
    this.framerateDenominator = 1,
    this.keyframeInterval = 0,
    this.sampleRate = 48000,
    this.channels = ChannelLayout.stereo,
    this.sampleFormat = SampleFormat.s16,
    this.frameDurationMs = 0,
    this.bitrate = 0,
    this.quality = 0,
    this.preset = Preset.balanced,
    this.options = const <String, String>{},
  });

  /// A picture encoder configuration.
  ///
  /// [width] and [height] must be even and non-zero (the Rust side rejects odd
  /// dimensions), and either [bitrate] or [quality] must be non-zero.
  /// [options] carries backend-specific key/value pairs (`"profile": "high"`);
  /// they are passed through untouched.
  factory EncoderConfig.video({
    required Codec codec,
    required int width,
    required int height,
    PixelFormat pixelFormat = PixelFormat.i420,
    int framerateNumerator = 30,
    int framerateDenominator = 1,
    int keyframeInterval = 0,
    int bitrate = 0,
    double quality = 0,
    Preset preset = Preset.balanced,
    Map<String, String> options = const <String, String>{},
  }) {
    if (!codec.isVideo) {
      throw ArgumentError.value(codec, 'codec', 'is not a picture codec');
    }
    return EncoderConfig(
      codec: codec,
      width: width,
      height: height,
      pixelFormat: pixelFormat,
      framerateNumerator: framerateNumerator,
      framerateDenominator: framerateDenominator,
      keyframeInterval: keyframeInterval,
      bitrate: bitrate,
      quality: quality,
      preset: preset,
      options: options,
    );
  }

  /// A sound encoder configuration.
  ///
  /// [frameDurationMs] is the requested encoded frame length; `0` lets the
  /// backend choose.
  factory EncoderConfig.audio({
    required Codec codec,
    int sampleRate = 48000,
    ChannelLayout channels = ChannelLayout.stereo,
    SampleFormat sampleFormat = SampleFormat.s16,
    int bitrate = 0,
    double quality = 0,
    int frameDurationMs = 0,
    Preset preset = Preset.balanced,
    Map<String, String> options = const <String, String>{},
  }) {
    if (!codec.isAudio) {
      throw ArgumentError.value(codec, 'codec', 'is not a sound codec');
    }
    return EncoderConfig(
      codec: codec,
      sampleRate: sampleRate,
      channels: channels,
      sampleFormat: sampleFormat,
      frameDurationMs: frameDurationMs,
      bitrate: bitrate,
      quality: quality,
      preset: preset,
      options: options,
    );
  }

  /// The codec to produce.
  final Codec codec;

  /// Luma width in pixels; ignored for sound codecs.
  final int width;

  /// Luma height in pixels; ignored for sound codecs.
  final int height;

  /// Input picture layout; must be valid even for a sound codec.
  final PixelFormat pixelFormat;

  /// Frames per second numerator.
  final int framerateNumerator;

  /// Frames per second denominator; the ABI clamps it to at least `1`.
  final int framerateDenominator;

  /// Target keyframe distance in frames; `0` lets the backend choose.
  final int keyframeInterval;

  /// Samples per second; ignored for picture codecs.
  final int sampleRate;

  /// Input speaker arrangement; must be valid even for a picture codec.
  final ChannelLayout channels;

  /// Input sample layout; must be valid even for a picture codec.
  final SampleFormat sampleFormat;

  /// Requested encoded frame length in milliseconds; `0` lets the backend choose.
  final int frameDurationMs;

  /// Target bitrate in bits per second; `0` means "use [quality]".
  final int bitrate;

  /// Quality hint in `0.0..=1.0`, or `0.0` when unset.
  final double quality;

  /// Speed/size trade-off.
  final Preset preset;

  /// Backend-specific option pairs, passed through verbatim.
  final Map<String, String> options;

  /// The same configuration with a different option map.
  EncoderConfig withOptions(Map<String, String> options) => EncoderConfig(
    codec: codec,
    width: width,
    height: height,
    pixelFormat: pixelFormat,
    framerateNumerator: framerateNumerator,
    framerateDenominator: framerateDenominator,
    keyframeInterval: keyframeInterval,
    sampleRate: sampleRate,
    channels: channels,
    sampleFormat: sampleFormat,
    frameDurationMs: frameDurationMs,
    bitrate: bitrate,
    quality: quality,
    preset: preset,
    options: options,
  );

  @override
  String toString() => codec.isVideo
      ? 'EncoderConfig(${codec.name} ${width}x$height ${pixelFormat.name} '
            '$framerateNumerator/$framerateDenominator fps bitrate=$bitrate '
            'quality=$quality ${preset.name})'
      : 'EncoderConfig(${codec.name} ${sampleRate}Hz ${channels.channels}ch '
            '${sampleFormat.name} bitrate=$bitrate quality=$quality)';
}

/// Everything needed to start a decoder.
///
/// `null` for [pixelFormat], [channels] and [sampleFormat] means "whatever the
/// backend prefers"; the ABI spells that as `0`.
final class DecoderConfig {
  /// Builds a decoder configuration.
  ///
  /// [extradata] is the codec configuration blob (`avcC`, `hvcC`,
  /// `AudioSpecificConfig`, `OpusHead`, ...) when the container supplies one.
  DecoderConfig({
    required this.codec,
    this.extradata,
    this.width = 0,
    this.height = 0,
    this.pixelFormat,
    this.sampleRate = 0,
    this.channels,
    this.sampleFormat,
    this.annexB = false,
  });

  /// The codec to consume.
  final Codec codec;

  /// Codec configuration bytes, or `null` when the bitstream carries them.
  final Uint8List? extradata;

  /// Coded width hint, or `0` when unknown.
  final int width;

  /// Coded height hint, or `0` when unknown.
  final int height;

  /// Requested output picture layout, or `null` for the backend default.
  final PixelFormat? pixelFormat;

  /// Samples-per-second hint, or `0` when unknown.
  final int sampleRate;

  /// Requested output speaker arrangement, or `null` for the backend default.
  final ChannelLayout? channels;

  /// Requested output sample layout, or `null` for the backend default.
  final SampleFormat? sampleFormat;

  /// `true` when the bitstream uses Annex-B start codes rather than
  /// length-prefixed NAL units.
  final bool annexB;

  /// The same configuration with different codec configuration bytes.
  DecoderConfig withExtradata(Uint8List? extradata) => DecoderConfig(
    codec: codec,
    extradata: extradata,
    width: width,
    height: height,
    pixelFormat: pixelFormat,
    sampleRate: sampleRate,
    channels: channels,
    sampleFormat: sampleFormat,
    annexB: annexB,
  );

  @override
  String toString() =>
      'DecoderConfig(${codec.name}, ${extradata?.length ?? 0} bytes extradata, '
      'annexB=$annexB)';
}
