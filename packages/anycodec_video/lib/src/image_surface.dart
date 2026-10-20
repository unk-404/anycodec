// The geometry fields are private but the parameters are public and named,
// so an initializing formal would expose `_width` as the argument name.
// ignore_for_file: prefer_initializing_formals

import 'dart:async';
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/widgets.dart';

import 'surface.dart';

/// A surface backed by `dart:ui` images.
///
/// Every presented frame is decoded into a `ui.Image` and drawn with `RawImage`.
/// That costs one copy and one texture upload per frame, and in exchange it works
/// on **every** platform with no native code at all — including the web, where
/// there is no external-texture registrar.
///
/// This is the fallback that makes the API honest: a caller can always show video,
/// even on a platform where the zero-copy path has not been implemented, and
/// [path] tells them which one they got.
final class ImageVideoSurface implements AnycodecVideoSurface {
  /// Creates an image surface.
  ImageVideoSurface({
    required int width,
    required int height,
    VideoPixelFormat format = VideoPixelFormat.rgba8888,
    String? fallbackReason,
  }) : _width = width,
       _height = height,
       _format = format,
       _path = VideoSurfacePath(
         backend: VideoSurfaceBackend.platformImage,
         zeroCopy: false,
         detail: 'dart:ui image (${format.name}, one copy + one upload/frame)',
         fallbackReason: fallbackReason,
       );

  int _width;
  int _height;
  final VideoPixelFormat _format;
  final VideoSurfacePath _path;
  final ValueNotifier<ui.Image?> _image = ValueNotifier<ui.Image?>(null);
  Future<void>? _pending;
  bool _disposed = false;

  @override
  int get width => _width;

  @override
  int get height => _height;

  @override
  VideoPixelFormat get format => _format;

  @override
  VideoSurfacePath get path => _path;

  @override
  bool get isDisposed => _disposed;

  @override
  Widget build({
    Key? key,
    BoxFit fit = BoxFit.contain,
    FilterQuality filterQuality = FilterQuality.low,
  }) {
    return ValueListenableBuilder<ui.Image?>(
      valueListenable: _image,
      builder: (context, image, _) {
        if (image == null) {
          // An empty surface is a black rectangle rather than nothing: a player
          // that reserves its space avoids a layout jump when the first frame
          // arrives.
          return SizedBox(
            key: key,
            width: _width.toDouble(),
            height: _height.toDouble(),
            child: const ColoredBox(color: Color(0xFF000000)),
          );
        }
        return RawImage(
          key: key,
          image: image,
          width: _width.toDouble(),
          height: _height.toDouble(),
          fit: fit,
          filterQuality: filterQuality,
        );
      },
    );
  }

  @override
  Future<void> present(Uint8List pixels) {
    _checkLive();
    final expected = _width * _height * 4;
    if (pixels.lengthInBytes != expected) {
      throw ArgumentError.value(
        pixels.lengthInBytes,
        'pixels',
        'expected $expected bytes for ${_width}x$_height (${_format.name})',
      );
    }
    // Serialise uploads: two frames decoding concurrently would otherwise finish
    // out of order and show an older frame after a newer one.
    final previous = _pending;
    final next = Completer<void>();
    _pending = next.future;
    () async {
      if (previous != null) {
        await previous;
      }
      if (_disposed) {
        next.complete();
        return;
      }
      final buffer = await ui.ImmutableBuffer.fromUint8List(pixels);
      final descriptor = ui.ImageDescriptor.raw(
        buffer,
        width: _width,
        height: _height,
        pixelFormat: _format == VideoPixelFormat.rgba8888
            ? ui.PixelFormat.rgba8888
            : ui.PixelFormat.bgra8888,
      );
      final codec = await descriptor.instantiateCodec();
      final frame = await codec.getNextFrame();
      codec.dispose();
      descriptor.dispose();
      buffer.dispose();
      if (_disposed) {
        frame.image.dispose();
        next.complete();
        return;
      }
      final old = _image.value;
      _image.value = frame.image;
      old?.dispose();
      next.complete();
    }();
    return next.future;
  }

  @override
  Future<void> resize(int width, int height) async {
    _checkLive();
    if (width == _width && height == _height) {
      return;
    }
    _width = width;
    _height = height;
    final old = _image.value;
    _image.value = null;
    old?.dispose();
  }

  @override
  Future<void> dispose() async {
    if (_disposed) {
      return;
    }
    _disposed = true;
    final old = _image.value;
    _image.value = null;
    old?.dispose();
    _image.dispose();
  }

  void _checkLive() {
    if (_disposed) {
      throw StateError('this video surface has been disposed');
    }
  }
}
