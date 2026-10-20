/// Web-specific extras: what the browser's WebCodecs implementation can do.
///
/// This is the file `package:anycodec/anycodec_platform.dart` exposes on the web
/// instead of the native extras. It is also what `runtime_web.dart` builds its
/// capability report from, so there is exactly one place that knows how to turn
/// a [Codec] into a WebCodecs codec string.
///
/// Nothing here imports `dart:ffi`; the native side of the package is not
/// reachable from a browser build by construction.
library;

import 'dart:async';
import 'dart:js_interop';
import 'dart:js_interop_unsafe';
import 'dart:typed_data';

import 'package:web/web.dart' as web;

import '../model/codec.dart';
import '../model/config.dart';
import '../model/format.dart';

/// The codec string WebCodecs uses for [codec], or `null` when the API has no
/// name for it.
///
/// H.266/VVC has no registered WebCodecs string in this binding, which is why
/// the web runtime reports it as unavailable rather than guessing.
String? webCodecStringFor(Codec codec) => switch (codec) {
  Codec.h264 => 'avc1.42001f',
  Codec.h265 => 'hev1.1.6.L93.B0',
  Codec.vp8 => 'vp8',
  Codec.vp9 => 'vp09.00.10.08',
  Codec.av1 => 'av01.0.04M.08',
  _ => null,
};

/// The WebCodecs pixel-format name for [format], or `null` when WebCodecs has no
/// equivalent.
String? webPixelFormatFor(PixelFormat format) => switch (format) {
  PixelFormat.i420 => 'I420',
  PixelFormat.nv12 => 'NV12',
  PixelFormat.rgba8 => 'RGBA',
  PixelFormat.bgra8 => 'BGRA',
  _ => null,
};

/// The `hardwareAcceleration` hint implied by [preference].
///
/// WebCodecs treats this as a *hint*: it cannot create hardware that is not
/// there, and it never reports what it actually used, which is why a web
/// session's `CodecPath.hardwareAccelerated` only follows
/// [BackendPreference.requireHardware] rather than claiming knowledge.
String webHardwareHint(BackendPreference preference) => switch (preference) {
  BackendPreference.requireHardware => 'prefer-hardware',
  BackendPreference.softwareOnly => 'prefer-software',
  _ => 'no-preference',
};

/// The answer to "can this browser configure this codec?".
final class WebCodecsConfigSupport {
  /// Builds a probe result.
  const WebCodecsConfigSupport({
    required this.supported,
    required this.codecString,
    required this.note,
  });

  /// `true` when `isConfigSupported` accepted the configuration.
  final bool supported;

  /// The codec string that was probed, or `null` when there is none.
  final String? codecString;

  /// A human readable explanation, suitable for a capability report.
  final String note;

  @override
  String toString() =>
      'WebCodecsConfigSupport($supported, $codecString, "$note")';
}

/// One codec's WebCodecs support, in both directions.
final class WebCodecsSupport {
  /// Builds a per-codec support entry.
  const WebCodecsSupport({
    required this.codec,
    required this.videoEncode,
    required this.videoDecode,
    required this.note,
  });

  /// The codec.
  final Codec codec;

  /// `true` when `VideoEncoder.isConfigSupported` accepted it.
  final bool videoEncode;

  /// `true` when `VideoDecoder.isConfigSupported` accepted it.
  final bool videoDecode;

  /// Why, when either direction is unsupported.
  final String note;

  /// `true` when the codec can be used in at least one direction.
  bool get supported => videoEncode || videoDecode;

  @override
  String toString() =>
      'WebCodecsSupport(${codec.name}, encode=$videoEncode, decode=$videoDecode)';
}

/// WebCodecs capability and configuration probing.
///
/// Every method here is safe to call on a browser that has no WebCodecs at all:
/// the first call throws, and the probing methods turn that into
/// `supported == false` with the reason in [WebCodecsConfigSupport.note].
abstract final class WebCodecs {
  /// `true` when the browser exposes the WebCodecs video interfaces.
  ///
  /// Checked by looking the constructors up on the global object rather than by
  /// assuming, because a browser without WebCodecs would otherwise fail with a
  /// `TypeError` at the first call instead of a clean report.
  static bool get videoSupported {
    try {
      return globalContext.has('VideoEncoder') &&
          globalContext.has('VideoDecoder');
    } catch (_) {
      return false;
    }
  }

  /// `true` when this binding exposes WebCodecs audio interfaces.
  ///
  /// Always `false`: `package:web` 1.1.1 has no `AudioEncoder`/`AudioDecoder`
  /// types, so the web runtime refuses audio sessions instead of pretending.
  static bool get audioSupported => false;

  /// Probes a video encoder configuration.
  static Future<WebCodecsConfigSupport> supportsVideoEncoder({
    required Codec codec,
    required int width,
    required int height,
    int bitrate = 1000000,
    double framerate = 30,
    BackendPreference preference = BackendPreference.auto,
  }) async {
    final codecString = webCodecStringFor(codec);
    if (codecString == null) {
      return WebCodecsConfigSupport(
        supported: false,
        codecString: null,
        note: 'WebCodecs has no codec string for ${codec.name}',
      );
    }
    try {
      final support = await web.VideoEncoder.isConfigSupported(
        web.VideoEncoderConfig(
          codec: codecString,
          width: width,
          height: height,
          bitrate: bitrate,
          framerate: framerate,
          hardwareAcceleration: webHardwareHint(preference),
        ),
      ).toDart;
      return WebCodecsConfigSupport(
        supported: support.supported,
        codecString: codecString,
        note: support.supported
            ? 'ready (the browser reports $codecString as supported)'
            : 'the browser rejected $codecString at ${width}x$height',
      );
    } catch (error) {
      return WebCodecsConfigSupport(
        supported: false,
        codecString: codecString,
        note: 'the WebCodecs probe failed: $error',
      );
    }
  }

  /// Probes a video decoder configuration.
  static Future<WebCodecsConfigSupport> supportsVideoDecoder({
    required Codec codec,
    int codedWidth = 640,
    int codedHeight = 480,
    Uint8List? description,
    BackendPreference preference = BackendPreference.auto,
  }) async {
    final codecString = webCodecStringFor(codec);
    if (codecString == null) {
      return WebCodecsConfigSupport(
        supported: false,
        codecString: null,
        note: 'WebCodecs has no codec string for ${codec.name}',
      );
    }
    try {
      final config = web.VideoDecoderConfig(
        codec: codecString,
        codedWidth: codedWidth,
        codedHeight: codedHeight,
        hardwareAcceleration: webHardwareHint(preference),
      );
      if (description != null) {
        config.description = description.toJS;
      }
      final support = await web.VideoDecoder.isConfigSupported(config).toDart;
      return WebCodecsConfigSupport(
        supported: support.supported,
        codecString: codecString,
        note: support.supported
            ? 'ready (the browser reports $codecString as supported)'
            : 'the browser rejected $codecString',
      );
    } catch (error) {
      return WebCodecsConfigSupport(
        supported: false,
        codecString: codecString,
        note: 'the WebCodecs probe failed: $error',
      );
    }
  }

  /// Probes every codec in both directions at a nominal 640x480.
  ///
  /// Audio codecs always come back unsupported, with the reason: this binding
  /// has no audio WebCodecs types.
  static Future<List<WebCodecsSupport>> capabilities({
    BackendPreference preference = BackendPreference.auto,
  }) async {
    final result = <WebCodecsSupport>[];
    for (final codec in Codec.values) {
      if (!codec.isVideo) {
        result.add(
          WebCodecsSupport(
            codec: codec,
            videoEncode: false,
            videoDecode: false,
            note:
                'the WebCodecs audio bindings are not available in '
                'package:web 1.1.1',
          ),
        );
        continue;
      }
      final encode = await supportsVideoEncoder(
        codec: codec,
        width: 640,
        height: 480,
        preference: preference,
      );
      final decode = await supportsVideoDecoder(
        codec: codec,
        preference: preference,
      );
      result.add(
        WebCodecsSupport(
          codec: codec,
          videoEncode: encode.supported,
          videoDecode: decode.supported,
          note: encode.supported || decode.supported
              ? 'ready (${encode.codecString ?? decode.codecString})'
              : '${encode.note}; ${decode.note}',
        ),
      );
    }
    return result;
  }
}
