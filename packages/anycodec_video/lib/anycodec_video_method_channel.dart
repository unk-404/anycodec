import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import 'anycodec_video_platform_interface.dart';
import 'src/surface.dart';

/// An implementation of [AnycodecVideoPlatform] that uses method channels.
///
/// The channel name and the method names are the contract the native hosts
/// implement; `windows/anycodec_video_plugin.cpp` implements them, and the other
/// hosts currently do not, which is why `AnycodecVideo.create` always checks
/// [supportsTexture] first instead of assuming.
class MethodChannelAnycodecVideo extends AnycodecVideoPlatform {
  /// The method channel used to interact with the native platform.
  @visibleForTesting
  final methodChannel = const MethodChannel('anycodec_video');

  @override
  Future<bool> supportsTexture() async {
    try {
      final supported = await methodChannel.invokeMethod<bool>(
        'supportsTexture',
      );
      return supported ?? false;
    } on MissingPluginException {
      // The host has no implementation at all. That is the normal state on the
      // platforms this plugin has not been ported to yet, not an error.
      return false;
    }
  }

  @override
  Future<int> createTexture({
    required int width,
    required int height,
    required VideoPixelFormat format,
  }) async {
    final id = await methodChannel.invokeMethod<int>('createTexture', {
      'width': width,
      'height': height,
      'format': format.name,
    });
    if (id == null) {
      throw StateError('the native host returned no texture id');
    }
    return id;
  }

  @override
  Future<void> updateTexture(int textureId, Uint8List pixels) {
    return methodChannel.invokeMethod<void>('updateTexture', {
      'textureId': textureId,
      'pixels': pixels,
    });
  }

  @override
  Future<void> resizeTexture(
    int textureId, {
    required int width,
    required int height,
  }) {
    return methodChannel.invokeMethod<void>('resizeTexture', {
      'textureId': textureId,
      'width': width,
      'height': height,
    });
  }

  @override
  Future<void> disposeTexture(int textureId) {
    return methodChannel.invokeMethod<void>('disposeTexture', {
      'textureId': textureId,
    });
  }
}
