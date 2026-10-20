import 'dart:typed_data';

import 'package:flutter/widgets.dart';

/// How the four bytes of one pixel are ordered.
///
/// Both orders exist because the natural output differs by platform: Windows
/// `DIB`/`DXGI` and Android `Bitmap` are BGRA, everything else is RGBA.
enum VideoPixelFormat {
  /// Red, green, blue, alpha — byte order `R G B A`.
  rgba8888,

  /// Blue, green, red, alpha — byte order `B G R A`.
  bgra8888;

  /// Bytes per pixel. Only 4-byte formats exist; 10-bit and YUV are `anycodec`'s
  /// decoded output, not a presentation format.
  int get bytesPerPixel => 4;
}

/// Which mechanism is actually putting pixels on the screen.
enum VideoSurfaceBackend {
  /// A Flutter external texture. The engine samples the buffer we write, so the
  /// pixels are uploaded by the raster thread rather than copied through Dart.
  nativeTexture,

  /// A `dart:ui` image, re-uploaded every frame. Works on every platform
  /// including the web, at the cost of one copy plus one upload per frame.
  platformImage,
}

/// What a surface is really doing, in the same spirit as `anycodec`'s `CodecPath`.
///
/// The whole point is that the caller never has to guess: a texture is a
/// platform-specific privilege, and a request for one can silently become an
/// image upload on a platform that does not implement it. That is fine as long as
/// it is *reported*, which is what [backend] and [fallbackReason] are for.
final class VideoSurfacePath {
  /// Creates a path description.
  const VideoSurfacePath({
    required this.backend,
    required this.zeroCopy,
    required this.detail,
    this.fallbackReason,
  });

  /// The mechanism in use.
  final VideoSurfaceBackend backend;

  /// `true` when the caller's pixels reach the engine without a Dart-side copy.
  ///
  /// This is `false` for [VideoSurfaceBackend.platformImage] by construction, and
  /// also `false` for a texture surface whose `present` takes bytes through a
  /// platform channel.
  final bool zeroCopy;

  /// A short, specific description of what is being used.
  final String detail;

  /// Why a better backend was not used, or `null` when the preferred one was.
  final String? fallbackReason;

  /// `true` when an external texture is in use.
  bool get isNativeTexture => backend == VideoSurfaceBackend.nativeTexture;

  /// `true` when a faster backend exists but was not available.
  bool get isFallback => fallbackReason != null;

  /// One line suitable for a log or a diagnostics panel.
  String get summary => isFallback
      ? '${backend.name} ($detail) — fell back: $fallbackReason'
      : '${backend.name} ($detail)';

  @override
  String toString() => 'VideoSurfacePath($summary)';
}

/// Something that can display decoded video frames.
///
/// This is deliberately *not* a player. It has no clock, no frame queue, no
/// audio, no seeking and no idea what a duration is. A player is the caller's
/// loop: decode a frame, call [present], repeat — and decide for itself when to
/// drop a frame or hold one back.
///
/// Frames are whole images: `width * height * 4` bytes in [format]. A surface
/// does not accept partial updates, because a frame that tears across the middle
/// is worse than one that arrives a millisecond late.
abstract interface class AnycodecVideoSurface {
  /// Width in pixels of the current frame.
  int get width;

  /// Height in pixels of the current frame.
  int get height;

  /// Byte order expected by [present].
  VideoPixelFormat get format;

  /// What this surface is actually doing.
  VideoSurfacePath get path;

  /// `true` once [dispose] has run.
  bool get isDisposed;

  /// A widget that shows the most recently presented frame.
  ///
  /// The surface does not impose a size or a layout: the caller decides whether
  /// it fills the screen, sits in a corner, or is inside a `Stack` with
  /// controls on top. [fit] and [filterQuality] are passed through.
  Widget build({
    Key? key,
    BoxFit fit = BoxFit.contain,
    FilterQuality filterQuality = FilterQuality.low,
  });

  /// Publishes one frame.
  ///
  /// [pixels] must be exactly `width * height * 4` bytes; anything else throws
  /// [ArgumentError] rather than drawing a torn or sheared image.
  Future<void> present(Uint8List pixels);

  /// Changes the frame size. The next [present] must match the new size.
  Future<void> resize(int width, int height);

  /// Releases whatever native resource this surface holds.
  ///
  /// Idempotent. After it, [present] throws.
  Future<void> dispose();
}
