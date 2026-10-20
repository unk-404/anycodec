// ignore: avoid_web_libraries_in_flutter

import 'package:flutter_web_plugins/flutter_web_plugins.dart';

import 'anycodec_video_platform_interface.dart';

/// The web implementation of [AnycodecVideoPlatform].
///
/// The web has no external-texture registrar: the engine composites from DOM or
/// canvas, and a plugin cannot hand it a pixel buffer to sample. So this
/// implementation deliberately does **not** override `supportsTexture`, which
/// means `AnycodecVideo.create` sees `false` and uses the `dart:ui` image path —
/// which does work in a browser, because `decodeImageFromPixels`/`ImageDescriptor`
/// are part of the engine there too.
///
/// That is the honest split: no fake texture, no per-platform surprise, and the
/// caller is told through `VideoSurfacePath.fallbackReason`.
class AnycodecVideoWeb extends AnycodecVideoPlatform {
  /// Constructs a AnycodecVideoWeb.
  AnycodecVideoWeb();

  /// Registers this implementation when the plugin is loaded on the web.
  static void registerWith(Registrar registrar) {
    AnycodecVideoPlatform.instance = AnycodecVideoWeb();
  }
}
