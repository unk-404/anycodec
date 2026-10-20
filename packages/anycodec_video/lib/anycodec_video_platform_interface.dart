import 'dart:typed_data';

import 'package:plugin_platform_interface/plugin_platform_interface.dart';

import 'anycodec_video_method_channel.dart';
import 'src/surface.dart';

/// The platform seam behind `anycodec_video`.
///
/// Everything here is optional on purpose. A platform that cannot register
/// external textures returns `false` from [supportsTexture] and the Dart layer
/// quietly uses `ImageVideoSurface` instead, reporting why through
/// `VideoSurfacePath.fallbackReason`. That is what lets one API work on six
/// platforms while only some of them are fast.
///
/// Only [supportsTexture] has a default. The rest throw, because a platform that
/// claims support and then fails is a bug worth seeing, not something to swallow.
abstract class AnycodecVideoPlatform extends PlatformInterface {
  /// Constructs a AnycodecVideoPlatform.
  AnycodecVideoPlatform() : super(token: _token);

  static final Object _token = Object();

  static AnycodecVideoPlatform _instance = MethodChannelAnycodecVideo();

  /// The default instance of [AnycodecVideoPlatform] to use.
  static AnycodecVideoPlatform get instance => _instance;

  /// Platform-specific implementations should set this with their own class.
  static set instance(AnycodecVideoPlatform instance) {
    PlatformInterface.verifyToken(instance, _token);
    _instance = instance;
  }

  /// Whether this platform can register an external texture at all.
  ///
  /// Defaults to `false`, which is the safe answer: the image surface then takes
  /// over and the caller is told.
  Future<bool> supportsTexture() async => false;

  /// Allocates a `width * height` texture and returns its engine id.
  Future<int> createTexture({
    required int width,
    required int height,
    required VideoPixelFormat format,
  }) {
    throw UnimplementedError('createTexture() is not implemented.');
  }

  /// Copies [pixels] into the texture and asks the engine to redraw it.
  Future<void> updateTexture(int textureId, Uint8List pixels) {
    throw UnimplementedError('updateTexture() is not implemented.');
  }

  /// Reallocates the texture at a new size, keeping its id.
  Future<void> resizeTexture(
    int textureId, {
    required int width,
    required int height,
  }) {
    throw UnimplementedError('resizeTexture() is not implemented.');
  }

  /// Releases the texture.
  Future<void> disposeTexture(int textureId) {
    throw UnimplementedError('disposeTexture() is not implemented.');
  }
}
