/// Texture and image presentation for `anycodec`.
///
/// This package hands decoded video frames to the screen and stops there. It has
/// no clock, no frame queue, no audio and no controls — a player is the caller's
/// loop around [AnycodecVideoSurface.present].
///
/// There are two ways to present, and [AnycodecVideo.create] chooses one and
/// **tells you which**:
///
/// | Backend | Mechanism | Where |
/// |---|---|---|
/// | `nativeTexture` | a Flutter external texture | platforms whose host implements the registrar |
/// | `platformImage` | a `dart:ui` image, re-uploaded | everywhere, including the web |
///
/// A request for a texture is a *request*: if the platform does not implement
/// one, the image path takes over and `VideoSurfacePath.fallbackReason` says why.
///
/// ```dart
/// final surface = await AnycodecVideo.create(width: 1280, height: 720);
/// print(surface.path.summary); // e.g. "platformImage (...) — fell back: ..."
/// // ... decode a frame into `pixels` yourself ...
/// await surface.present(pixels);
/// // and put `surface.build()` wherever it should appear
/// ```
library;

import 'src/image_surface.dart';
import 'src/surface.dart';
import 'src/texture_surface.dart';

export 'anycodec_video_platform_interface.dart'
    show AnycodecVideoPlatform;
export 'src/image_surface.dart' show ImageVideoSurface;
export 'src/surface.dart';
export 'src/texture_surface.dart' show TextureVideoSurface;

import 'anycodec_video_platform_interface.dart';

/// Creates video surfaces.
abstract final class AnycodecVideo {
  /// Creates a surface for frames of [width] x [height].
  ///
  /// [preferNativeTexture] asks for the fast path. It is a request, not a
  /// guarantee: when the platform cannot register a texture the image path is
  /// used and [AnycodecVideoSurface.path] reports the reason. Pass `false` to
  /// force the portable path, which is useful for A/B performance comparisons.
  ///
  /// Throws [ArgumentError] for a non-positive size; a zero-sized surface cannot
  /// be presented to and would fail on the first frame instead of here.
  static Future<AnycodecVideoSurface> create({
    required int width,
    required int height,
    VideoPixelFormat format = VideoPixelFormat.rgba8888,
    bool preferNativeTexture = true,
  }) async {
    if (width <= 0 || height <= 0) {
      throw ArgumentError.value(
        '$width x $height',
        'size',
        'must be positive in both dimensions',
      );
    }
    if (!preferNativeTexture) {
      return ImageVideoSurface(width: width, height: height, format: format);
    }
    final platform = AnycodecVideoPlatform.instance;
    if (!await platform.supportsTexture()) {
      return ImageVideoSurface(
        width: width,
        height: height,
        format: format,
        fallbackReason: 'this platform does not implement external textures',
      );
    }
    try {
      final textureId = await platform.createTexture(
        width: width,
        height: height,
        format: format,
      );
      return TextureVideoSurface(
        textureId: textureId,
        width: width,
        height: height,
        format: format,
      );
    } catch (error) {
      // A platform that advertises textures and then fails is worth knowing
      // about, so the reason is kept rather than the failure being retried
      // silently.
      return ImageVideoSurface(
        width: width,
        height: height,
        format: format,
        fallbackReason: 'texture creation failed: $error',
      );
    }
  }

  /// Whether the current platform implements external textures.
  ///
  /// Cheap enough to call before showing a settings toggle.
  static Future<bool> get supportsNativeTexture =>
      AnycodecVideoPlatform.instance.supportsTexture();
}
