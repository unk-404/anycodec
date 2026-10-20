/// Capability reports, hardware devices and the "did it fall back?" answer.
library;

import 'codec.dart';

/// Which subsystem performed (or could perform) the work.
///
/// The `raw` values are the `ANYCODEC_BACKEND_*` constants of the C ABI. The
/// WebCodecs entry is the one that never appears in a native report: on the web
/// it is the *only* implementation, and it lives in the Dart runtime rather than
/// in the Rust core.
enum Backend {
  /// Android `AMediaCodec`.
  mediaCodec(0, 'mediacodec'),

  /// Windows Media Foundation.
  mediaFoundation(1, 'mediafoundation'),

  /// Apple VideoToolbox.
  videoToolbox(2, 'videotoolbox'),

  /// Apple AudioToolbox.
  audioToolbox(3, 'audiotoolbox'),

  /// Linux GStreamer.
  gStreamer(4, 'gstreamer'),

  /// Browser WebCodecs.
  webCodecs(5, 'webcodecs'),

  /// Pure-Rust software codec.
  rust(6, 'rust'),

  /// Runtime-loaded `libavcodec`.
  ffmpeg(7, 'ffmpeg'),

  /// Application-supplied backend.
  external(8, 'external');

  const Backend(this.raw, this.token);

  /// The `ANYCODEC_BACKEND_*` value used by the C ABI.
  final int raw;

  /// Stable lower-case token, identical to the Rust `BackendKind::name`.
  final String token;

  /// The backend whose ABI identifier is [raw], or `null` when unknown.
  static Backend? fromRaw(int raw) {
    for (final backend in values) {
      if (backend.raw == raw) {
        return backend;
      }
    }
    return null;
  }

  /// `true` when this backend is a software fallback rather than a platform
  /// framework.
  bool get isSoftware => this == Backend.rust || this == Backend.ffmpeg;

  /// `true` when this backend can run on dedicated hardware.
  ///
  /// This is the backend's *capability*, not a session's state; the per-session
  /// truth is [CodecPath.hardwareAccelerated].
  bool get isHardwareCapable => switch (this) {
    Backend.mediaCodec ||
    Backend.mediaFoundation ||
    Backend.videoToolbox ||
    Backend.gStreamer ||
    Backend.webCodecs => true,
    _ => false,
  };

  /// `true` for the platform media frameworks (as opposed to [Backend.rust],
  /// [Backend.ffmpeg] and [Backend.external]).
  bool get isPlatform => switch (this) {
    Backend.rust || Backend.ffmpeg || Backend.external => false,
    _ => true,
  };
}

/// Whether the work was done by a platform framework or by a software codec.
enum Implementation {
  /// A platform media framework.
  native(0, 'native'),

  /// A software codec (pure Rust, or a dynamically loaded `libavcodec`).
  software(1, 'software');

  const Implementation(this.raw, this.token);

  /// The `ANYCODEC_IMPLEMENTATION_*` value used by the C ABI.
  final int raw;

  /// Stable lower-case token.
  final String token;

  /// The implementation whose ABI identifier is [raw], or `null` when unknown.
  static Implementation? fromRaw(int raw) {
    for (final implementation in values) {
      if (implementation.raw == raw) {
        return implementation;
      }
    }
    return null;
  }
}

/// Exactly how one live encoder or decoder is doing its work.
///
/// This is the answer to "did my encode silently fall back to software?" — check
/// [isSoftware], and read [fallbackReason] to find out which higher-priority
/// backend was skipped.
final class CodecPath {
  /// Builds a path description.
  const CodecPath({
    required this.backend,
    required this.implementation,
    this.hardwareAccelerated = false,
    this.detail = '',
    this.fallbackReason,
  });

  /// The subsystem doing the work.
  final Backend backend;

  /// Native framework or software fallback.
  final Implementation implementation;

  /// `true` only when the backend is *known* to run on dedicated hardware.
  final bool hardwareAccelerated;

  /// The concrete object in use, e.g. `"rust: passthrough (verbatim store)"`.
  final String detail;

  /// Why a higher-priority backend was not used, or `null` when none was
  /// skipped.
  final String? fallbackReason;

  /// `true` when a software fallback is doing the work.
  bool get isSoftware => implementation == Implementation.software;

  /// `true` when a platform framework is doing the work.
  bool get isNative => implementation == Implementation.native;

  /// `true` when something higher-priority was skipped to get here.
  bool get hasFallback => fallbackReason != null && fallbackReason!.isNotEmpty;

  /// One-line rendering, e.g.
  /// `rust (software): rust: passthrough (verbatim store)`.
  String get summary {
    final buffer = StringBuffer()
      ..write(backend.token)
      ..write(
        implementation == Implementation.software ? ' (software)' : ' (native)',
      );
    if (hardwareAccelerated) {
      buffer.write(', hardware');
    }
    if (detail.isNotEmpty) {
      buffer.write(': $detail');
    }
    return buffer.toString();
  }

  @override
  String toString() =>
      'CodecPath($summary${hasFallback ? ', fallback: $fallbackReason' : ''})';
}

/// What one backend reports about one codec.
final class BackendCapability {
  /// Builds one backend's line of the capability report.
  const BackendCapability({
    required this.backend,
    required this.implementation,
    required this.hardware,
    required this.available,
    required this.note,
  });

  /// The subsystem.
  final Backend backend;

  /// Native framework or software fallback.
  final Implementation implementation;

  /// `true` when this backend is known to use dedicated hardware.
  final bool hardware;

  /// `true` when the backend can be constructed right now.
  final bool available;

  /// `"ready"`, or the specific reason the backend is unusable.
  final String note;

  @override
  String toString() =>
      'BackendCapability(${backend.token}, ${implementation.token}, '
      'hardware=$hardware, available=$available, "$note")';
}

/// What is available for one codec in one direction.
final class CodecCapability {
  /// Builds a capability entry.
  const CodecCapability({
    required this.codec,
    required this.direction,
    required this.backends,
  });

  /// The codec this entry describes.
  final Codec codec;

  /// Whether this entry is about encoders or decoders.
  final CodecDirection direction;

  /// Every backend registered for this codec and direction, best first,
  /// available or not. Always non-empty for the twelve production codecs: an
  /// empty list would mean the codec is not implemented at all.
  final List<BackendCapability> backends;

  /// The backends that can be constructed right now.
  List<BackendCapability> get availableBackends => <BackendCapability>[
    for (final backend in backends)
      if (backend.available) backend,
  ];

  /// The backends that cannot be constructed right now, with their reasons.
  List<BackendCapability> get unavailableBackends => <BackendCapability>[
    for (final backend in backends)
      if (!backend.available) backend,
  ];

  /// `true` when at least one backend can run right now.
  bool get isAvailable => backends.any((backend) => backend.available);

  /// `true` when at least one *available* backend is a software fallback.
  bool get hasSoftwareFallback => backends.any(
    (backend) =>
        backend.available && backend.implementation == Implementation.software,
  );

  /// `true` when at least one available backend is known to use dedicated
  /// hardware.
  bool get hardwareAccelerated =>
      backends.any((backend) => backend.available && backend.hardware);

  /// The best available backend, or `null` when nothing can run.
  BackendCapability? get best => backends.isEmpty
      ? null
      : backends.firstWhere(
          (backend) => backend.available,
          orElse: () => backends.first,
        );

  /// One-line rendering used by [AnyCodec.capabilityReport] and the example app.
  String get summary {
    final buffer = StringBuffer()
      ..write(codec.name)
      ..write(direction == CodecDirection.encode ? ' encode: ' : ' decode: ');
    if (backends.isEmpty) {
      buffer.write('no backend registered');
      return buffer.toString();
    }
    buffer.write(
      backends
          .map(
            (backend) =>
                '${backend.backend.token}'
                '${backend.available ? '' : ' (${backend.note})'}',
          )
          .join(', '),
    );
    return buffer.toString();
  }

  @override
  String toString() => 'CodecCapability($summary)';
}

/// One codec a hardware engine supports.
final class HardwareDeviceCodec {
  /// Builds a device codec description.
  const HardwareDeviceCodec({
    required this.backend,
    required this.codec,
    required this.canDecode,
    required this.canEncode,
    this.maxWidth = 0,
    this.maxHeight = 0,
    this.maxBitDepth = 0,
  });

  /// The engine's subsystem.
  final Backend backend;

  /// The codec.
  final Codec codec;

  /// `true` when the engine can decode this codec.
  final bool canDecode;

  /// `true` when the engine can encode this codec.
  final bool canEncode;

  /// Largest supported luma width, or `0` when unknown.
  final int maxWidth;

  /// Largest supported luma height, or `0` when unknown.
  final int maxHeight;

  /// Bits per component, or `0` when unknown.
  final int maxBitDepth;

  @override
  String toString() =>
      'HardwareDeviceCodec(${codec.name}, encode=$canEncode, decode=$canDecode, '
      'max=${maxWidth}x$maxHeight)';
}

/// A hardware media engine found by `anycodec_probe_devices`.
final class HardwareDevice {
  /// Builds a device description.
  const HardwareDevice({
    required this.backend,
    required this.name,
    this.apiVersion = '',
    this.driverVersion = '',
    this.codecs = const <HardwareDeviceCodec>[],
  });

  /// The subsystem that owns the engine.
  final Backend backend;

  /// Engine name, as the platform reports it.
  final String name;

  /// API version string, when the platform exposes one.
  final String apiVersion;

  /// Driver version string, when the platform exposes one.
  final String driverVersion;

  /// The codecs the engine supports.
  final List<HardwareDeviceCodec> codecs;

  /// `true` when the engine can both encode and decode at least one codec.
  bool get isBiDirectional =>
      codecs.any((codec) => codec.canEncode) &&
      codecs.any((codec) => codec.canDecode);

  @override
  String toString() =>
      'HardwareDevice(${backend.token}, "$name", ${codecs.length} codecs)';
}
