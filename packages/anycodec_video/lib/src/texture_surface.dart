// The geometry fields are private but the parameters are public and named,
// so an initializing formal would expose `_width` as the argument name.
// ignore_for_file: prefer_initializing_formals

import 'dart:typed_data';

import 'package:flutter/widgets.dart';

import '../anycodec_video_platform_interface.dart';
import 'surface.dart';

/// A surface backed by a Flutter external texture.
///
/// This is the fast path: the native side owns a pixel buffer that the engine
/// samples directly, so presenting a frame is "write bytes, mark the texture
/// dirty" rather than "create a `ui.Image` and upload it". It exists only where
/// the platform implements the texture registrar, which is why [AnycodecVideo]
/// always treats it as a request rather than a guarantee.
///
/// The pixels still cross a platform channel on [present], so this is not
/// *zero-copy* from Dart's point of view — that would need the decoder to write
/// straight into the native buffer. What it removes is per-frame image and codec
/// object churn, which is the part that hurts in Dart.
final class TextureVideoSurface implements AnycodecVideoSurface {
  /// Creates a texture surface around an already registered texture.
  ///
  /// Prefer [AnycodecVideo.create], which registers the texture and falls back
  /// when the platform cannot.
  TextureVideoSurface({
    required int textureId,
    required int width,
    required int height,
    VideoPixelFormat format = VideoPixelFormat.rgba8888,
  }) : _textureId = textureId,
       _width = width,
       _height = height,
       _format = format;

  final int _textureId;
  int _width;
  int _height;
  final VideoPixelFormat _format;
  bool _disposed = false;
  Future<void> _pending = Future<void>.value();

  /// The engine-side texture id, for callers that want their own `Texture`.
  int get textureId => _textureId;

  @override
  int get width => _width;

  @override
  int get height => _height;

  @override
  VideoPixelFormat get format => _format;

  @override
  VideoSurfacePath get path => VideoSurfacePath(
    backend: VideoSurfaceBackend.nativeTexture,
    zeroCopy: false,
    detail: 'external texture #$_textureId '
        '(${_format.name}, one channel copy/frame)',
  );

  @override
  bool get isDisposed => _disposed;

  @override
  Widget build({
    Key? key,
    BoxFit fit = BoxFit.contain,
    FilterQuality filterQuality = FilterQuality.low,
  }) {
    // `Texture` always fills its box, so `fit` is honoured by the enclosing
    // `FittedBox` rather than by the widget itself.
    return ClipRect(
      key: key,
      child: FittedBox(
        fit: fit,
        child: SizedBox(
          width: _width.toDouble(),
          height: _height.toDouble(),
          child: Texture(textureId: _textureId, filterQuality: filterQuality),
        ),
      ),
    );
  }

  @override
  Future<void> present(Uint8List pixels) {
    if (_disposed) {
      throw StateError('this video surface has been disposed');
    }
    final expected = _width * _height * 4;
    if (pixels.lengthInBytes != expected) {
      throw ArgumentError.value(
        pixels.lengthInBytes,
        'pixels',
        'expected $expected bytes for ${_width}x$_height (${_format.name})',
      );
    }
    // Chain presentations so frames reach the engine in the order they were
    // produced; an out-of-order texture update shows a stale frame.
    _pending = _pending.then(
      (_) => AnycodecVideoPlatform.instance.updateTexture(_textureId, pixels),
    );
    return _pending;
  }

  @override
  Future<void> resize(int width, int height) async {
    if (_disposed) {
      throw StateError('this video surface has been disposed');
    }
    if (width == _width && height == _height) {
      return;
    }
    _width = width;
    _height = height;
    await AnycodecVideoPlatform.instance.resizeTexture(
      _textureId,
      width: width,
      height: height,
    );
  }

  @override
  Future<void> dispose() async {
    if (_disposed) {
      return;
    }
    _disposed = true;
    try {
      await _pending;
    } catch (_) {
      // A failed present must not prevent releasing the texture.
    }
    await AnycodecVideoPlatform.instance.disposeTexture(_textureId);
  }
}
