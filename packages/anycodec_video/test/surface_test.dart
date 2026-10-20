import 'dart:typed_data';

import 'package:anycodec_video/anycodec_video.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

/// A platform that claims external-texture support and then succeeds.
class _FakeTexturePlatform extends AnycodecVideoPlatform {
  int? created;
  int? disposed;
  int updates = 0;
  final List<Object?> failures = [];

  @override
  Future<bool> supportsTexture() async => true;

  @override
  Future<int> createTexture({
    required int width,
    required int height,
    required VideoPixelFormat format,
  }) async {
    final failure = failures.isEmpty ? null : failures.removeAt(0);
    if (failure != null) {
      throw failure;
    }
    created = 7000 + width; // a stable, obviously fake id
    return created!;
  }

  @override
  Future<void> updateTexture(int textureId, Uint8List pixels) async {
    updates++;
  }

  @override
  Future<void> disposeTexture(int textureId) async {
    disposed = textureId;
  }
}

/// A platform that honestly says it has no textures.
class _NoTexturePlatform extends AnycodecVideoPlatform {}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  tearDown(() {
    // Leave the default method-channel implementation in place for other tests.
    AnycodecVideoPlatform.instance = AnycodecVideoPlatform.instance;
  });

  group('AnycodecVideo.create', () {
    test('falls back to an image surface, and says why', () async {
      AnycodecVideoPlatform.instance = _NoTexturePlatform();
      final surface = await AnycodecVideo.create(width: 64, height: 48);
      expect(surface.path.backend, VideoSurfaceBackend.platformImage);
      expect(surface.path.isFallback, isTrue);
      expect(surface.path.fallbackReason, contains('external textures'));
      expect(surface.path.zeroCopy, isFalse);
      await surface.dispose();
    });

    test('uses a texture when the platform really has one', () async {
      final platform = _FakeTexturePlatform();
      AnycodecVideoPlatform.instance = platform;
      final surface = await AnycodecVideo.create(width: 64, height: 48);
      expect(surface, isA<TextureVideoSurface>());
      expect(surface.path.backend, VideoSurfaceBackend.nativeTexture);
      expect(surface.path.isFallback, isFalse);
      expect(platform.created, isNotNull);
      await surface.dispose();
      expect(platform.disposed, platform.created);
    });

    test('falls back and keeps the reason when texture creation fails', () async {
      final platform = _FakeTexturePlatform()
        ..failures.add(StateError('the GPU said no'));
      AnycodecVideoPlatform.instance = platform;
      final surface = await AnycodecVideo.create(width: 32, height: 32);
      expect(surface.path.backend, VideoSurfaceBackend.platformImage);
      expect(surface.path.fallbackReason, contains('the GPU said no'));
      await surface.dispose();
    });

    test('preferNativeTexture: false goes straight to the image path', () async {
      final platform = _FakeTexturePlatform();
      AnycodecVideoPlatform.instance = platform;
      final surface = await AnycodecVideo.create(
        width: 16,
        height: 16,
        preferNativeTexture: false,
      );
      expect(surface.path.backend, VideoSurfaceBackend.platformImage);
      expect(
        surface.path.isFallback,
        isFalse,
        reason: 'it was asked for, not fallen back to',
      );
      expect(platform.created, isNull);
      await surface.dispose();
    });

    test('rejects a zero or negative size instead of failing later', () async {
      AnycodecVideoPlatform.instance = _NoTexturePlatform();
      expect(
        () => AnycodecVideo.create(width: 0, height: 10),
        throwsArgumentError,
      );
      expect(
        () => AnycodecVideo.create(width: 10, height: -1),
        throwsArgumentError,
      );
    });
  });

  group('surface geometry', () {
    test('present rejects a frame of the wrong size', () async {
      AnycodecVideoPlatform.instance = _NoTexturePlatform();
      final surface = await AnycodecVideo.create(width: 4, height: 4);
      // 4x4x4 = 64 bytes is right; 63 is not, and must not be drawn sheared.
      expect(
        () => surface.present(Uint8List(63)),
        throwsArgumentError,
      );
      await surface.dispose();
    });

    test('a disposed surface refuses to present and disposes twice safely', () async {
      AnycodecVideoPlatform.instance = _NoTexturePlatform();
      final surface = await AnycodecVideo.create(width: 2, height: 2);
      await surface.dispose();
      expect(surface.isDisposed, isTrue);
      expect(() => surface.present(Uint8List(16)), throwsStateError);
      await surface.dispose(); // idempotent
    });

    test('resize updates the geometry and clears the frame', () async {
      AnycodecVideoPlatform.instance = _NoTexturePlatform();
      final surface = await AnycodecVideo.create(width: 8, height: 8);
      await surface.resize(4, 4);
      expect(surface.width, 4);
      expect(surface.height, 4);
      expect(
        () => surface.present(Uint8List(8 * 8 * 4)),
        throwsArgumentError,
        reason: 'the old geometry is no longer valid',
      );
      expect(() => surface.present(Uint8List(4 * 4 * 4)), returnsNormally);
      await surface.dispose();
    });

    test('build returns a widget with no frame yet', () async {
      AnycodecVideoPlatform.instance = _NoTexturePlatform();
      final surface = await AnycodecVideo.create(width: 3, height: 3);
      final widget = surface.build();
      expect(widget, isA<Widget>());
      await surface.dispose();
    });
  });
}
