/// The web runtime, exercised through the unified API.
///
/// These tests only mean something in a browser: the WebCodecs runtime exists
/// when `dart.library.js_interop` is available. On the VM the whole group is
/// reported as **skipped with a reason** rather than deleted or silently
/// passing. Run it for real with:
///
/// ```sh
/// dart test -p chrome test/web_runtime_test.dart
/// ```
///
/// Everything below is written against `package:anycodec/anycodec.dart` — the
/// unified API — because that is the API that must behave identically on the
/// web. The web-only extras (`WebCodecs`, `WebCodecsSupport`) are covered by
/// `test/web_compile_probe.dart`, which the browser compiler type-checks.
library;

import 'dart:typed_data';

import 'package:anycodec/anycodec.dart';
import 'package:test/test.dart';

import 'support/frames.dart';

/// `true` when this program is compiled for a JavaScript or Wasm runtime.
const bool isWebRuntime = bool.fromEnvironment('dart.library.js_interop');

void main() {
  group(
    'web runtime',
    () {
      test(
        'there are no isolates on the web, so there are no workers',
        () async {
          await AnyCodec.initialize();
          expect(AnyCodec.workerCount, 0);
        },
      );

      test('the diagnostic codec works in a browser with no media framework at all', () async {
        final encoder = await AnyCodec.diagnosticEncoder(Codec.h264);
        final decoder = await AnyCodec.diagnosticDecoder(Codec.h264);
        try {
          expect(encoder.path.isSoftware, isTrue);
          expect(encoder.path.detail, contains('passthrough'));
          final frame = sampleVideoFrame(pts: 4242);
          await encoder.encode(frame);
          final packet = await drainPacket(encoder);
          expect(packet, isNotNull);
          expect(packet!.pts, 4242);
          await decoder.decode(packet);
          final decoded = await drainFrame(decoder);
          expect(decoded, isA<VideoFrame>());
          final video = decoded! as VideoFrame;
          expect(video.width, frame.width);
          expect(video.pixelFormat, frame.pixelFormat);
          for (var index = 0; index < frame.planes.length; index++) {
            expect(video.planes[index], orderedEquals(frame.planes[index]));
          }
        } finally {
          await encoder.close();
          await decoder.close();
        }
      });

      test(
        'the capability report describes WebCodecs, not the Rust core',
        () async {
          final capabilities = await AnyCodec.capabilities();
          expect(capabilities.length, 24);
          for (final entry in capabilities) {
            expect(entry.backends, isNotEmpty);
            expect(entry.backends.first.backend, Backend.webCodecs);
          }
          final report = await AnyCodec.capabilityReport();
          expect(report, contains('h264'));
          expect(report, contains('opus'));
        },
      );

      test(
        'audio codecs are reported unsupported rather than silently missing',
        () async {
          final capabilities = await AnyCodec.capabilities();
          for (final entry in capabilities) {
            if (entry.codec.isVideo) {
              continue;
            }
            expect(entry.isAvailable, isFalse);
            expect(
              entry.backends.first.note,
              contains('audio'),
              reason: 'the report must say why, not just "no"',
            );
          }
        },
      );

      test('WebCodecs does not expose device enumeration', () async {
        expect(await AnyCodec.probeHardware(), isEmpty);
      });

      test('the diagnostic codec id matches the Rust codec slot', () {
        expect(AnyCodec.passthroughCodecId, Codec.h266.raw);
      });

      test('an audio session is an explicit unsupported error', () async {
        await expectLater(
          () => AnyCodec.encoder(EncoderConfig.audio(codec: Codec.opus)),
          throwsA(
            isA<AnyCodecException>().having(
              (error) => error.kind,
              'kind',
              AnyCodecErrorKind.unsupported,
            ),
          ),
        );
      });

      test(
        'a video session reports a WebCodecs path, or a clean refusal',
        () async {
          try {
            final encoder = await AnyCodec.encoder(
              EncoderConfig.video(
                codec: Codec.vp8,
                width: 320,
                height: 240,
                bitrate: 500000,
              ),
            );
            try {
              expect(encoder.path.backend, Backend.webCodecs);
              expect(encoder.path.detail, contains('VideoEncoder'));
              await encoder.encode(sampleVideoFrame(width: 320, height: 240));
              final packets = await encoder.flush();
              expect(
                packets,
                isNotEmpty,
                reason: 'VP8 encoding must produce a packet',
              );
              expect(packets.first.data, isNot(Uint8List(0)));
            } finally {
              await encoder.close();
            }
          } on AnyCodecException catch (error) {
            // A browser without a VP8 encoder is a legitimate environment; what must
            // not happen is an unclassified crash.
            expect(error.kind, AnyCodecErrorKind.unavailable);
          }
        },
      );

      tearDownAll(AnyCodec.shutdown);
    },
    skip: isWebRuntime
        ? null
        : 'the WebCodecs runtime only exists in a browser; run this suite with '
              '`dart test -p chrome test/web_runtime_test.dart`',
  );
}
