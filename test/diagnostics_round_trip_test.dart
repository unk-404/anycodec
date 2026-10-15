/// The diagnostic codec: a real encode/decode round trip through the binding.
///
/// The Rust side ships a `passthrough` codec that stores a frame verbatim and
/// retrieves it, precisely so that the whole pipeline — timestamps, buffer
/// ownership, the isolate hand-off, flush semantics and the C ABI itself — can
/// be exercised on a machine (or a browser) with no media framework at all. It
/// is deliberately not registered against any production codec.
///
/// This file drives it through the public API, and then through the raw
/// bindings where the public API cannot express the failure being tested (a NULL
/// plane).
library;

import 'dart:ffi';
import 'dart:typed_data';

import 'package:anycodec/anycodec.dart';
import 'package:anycodec/src/bindings/anycodec_bindings_generated.dart' as abi;
import 'package:anycodec/src/native/status.dart';
import 'package:anycodec/src/runtime/diagnostic_codec.dart';

import 'support/frames.dart';

import 'package:ffi/ffi.dart';
import 'package:test/test.dart';

void main() {
  setUpAll(AnyCodec.initialize);
  tearDownAll(AnyCodec.shutdown);

  group('through the public API', () {
    late AnycodecEncoder encoder;
    late AnycodecDecoder decoder;

    setUp(() async {
      encoder = await AnyCodec.diagnosticEncoder(Codec.h264);
      decoder = await AnyCodec.diagnosticDecoder(Codec.h264);
    });

    tearDown(() async {
      await encoder.close();
      await decoder.close();
    });

    test(
      'a picture keeps its geometry, format, timestamps and bytes',
      () async {
        final frame = sampleVideoFrame(pts: 123456);
        await encoder.encode(frame);
        final packet = await drainPacket(encoder);

        expect(packet, isNotNull, reason: 'the store encoder emits on push');
        expect(packet!.codec, Codec.h264);
        expect(packet.pts, frame.pts);
        expect(packet.length, greaterThan(0));
        expect(packet.flags.isKeyframe, isTrue);

        await decoder.decode(packet);
        final decoded = await drainFrame(decoder);
        expect(decoded, isA<VideoFrame>());
        final video = decoded! as VideoFrame;

        expect(video.width, frame.width);
        expect(video.height, frame.height);
        expect(video.pixelFormat, frame.pixelFormat);
        expect(video.pts, frame.pts);
        expect(video.strides, frame.strides);
        for (var index = 0; index < frame.planes.length; index++) {
          expect(
            video.planes[index],
            orderedEquals(frame.planes[index]),
            reason: 'plane $index must come back byte for byte',
          );
        }
      },
    );

    test('audio round-trips too', () async {
      final frame = sampleAudioFrame(pts: 42);
      await encoder.encode(frame);
      final packet = await drainPacket(encoder);
      expect(packet, isNotNull);
      await decoder.decode(packet!);
      final decoded = await drainFrame(decoder);
      expect(decoded, isA<AudioFrame>());
      final audio = decoded! as AudioFrame;
      expect(audio.sampleFormat, frame.sampleFormat);
      expect(audio.sampleRate, frame.sampleRate);
      expect(audio.channels, frame.channels);
      expect(audio.frames, frame.frames);
      expect(audio.pts, frame.pts);
      expect(audio.data, orderedEquals(frame.data));
    });

    test(
      'the reported path says "software" and names the passthrough codec',
      () {
        for (final path in <CodecPath>[encoder.path, decoder.path]) {
          expect(path.isSoftware, isTrue);
          expect(path.isNative, isFalse);
          expect(path.implementation, Implementation.software);
          expect(path.backend, Backend.rust);
          expect(path.hardwareAccelerated, isFalse);
          expect(path.detail, contains('passthrough'));
          expect(path.summary, contains('software'));
        }
      },
    );

    test(
      'bufferedPackets tracks what has not been drained, and flush drains it',
      () async {
        expect(await encoder.bufferedPackets(), 0);
        for (var index = 0; index < 3; index++) {
          await encoder.encode(sampleVideoFrame(pts: index * 1000));
        }
        expect(await encoder.bufferedPackets(), 3);

        final packets = await encoder.flush();
        expect(packets.length, 3);
        expect(await encoder.bufferedPackets(), 0);
        expect([for (final packet in packets) packet.pts], [0, 1000, 2000]);
        expect(
          await encoder.receive(),
          isNull,
          reason: 'the queue is empty now',
        );
      },
    );

    test('close is idempotent and a closed session refuses work', () async {
      await encoder.close();
      await encoder.close();
      expect(encoder.isClosed, isTrue);
      await expectLater(
        () => encoder.encode(sampleVideoFrame()),
        throwsA(
          isA<AnyCodecException>()
              .having((error) => error.kind, 'kind', AnyCodecErrorKind.state)
              .having((error) => error.operation, 'operation', 'encode'),
        ),
      );
      await expectLater(
        () => encoder.receive(),
        throwsA(isA<AnyCodecException>()),
      );

      await decoder.close();
      await decoder.close();
      await expectLater(
        () => decoder.decode(
          Packet(
            codec: Codec.h264,
            data: Uint8List.fromList([0, 1, 2, 3, 4, 5]),
          ),
        ),
        throwsA(isA<AnyCodecException>()),
      );
    });

    test('a truncated packet is a backend error, not a crash', () async {
      await expectLater(
        () => decoder.decode(
          Packet(codec: Codec.h264, data: Uint8List.fromList([1, 2])),
        ),
        throwsA(
          isA<AnyCodecException>().having(
            (error) => error.kind,
            'kind',
            AnyCodecErrorKind.backend,
          ),
        ),
      );
    });
  });

  group('the Dart-side guards that protect native memory', () {
    late AnycodecEncoder encoder;

    setUp(() async {
      encoder = await AnyCodec.diagnosticEncoder(Codec.h264);
    });

    tearDown(() => encoder.close());

    test('a frame with an undersized plane is rejected', () async {
      final frame = VideoFrame(
        pixelFormat: PixelFormat.i420,
        width: 64,
        height: 64,
        planes: <Uint8List>[Uint8List(16), Uint8List(4), Uint8List(4)],
      );
      await expectLater(
        () => encoder.encode(frame),
        throwsA(
          isA<AnyCodecException>()
              .having(
                (error) => error.kind,
                'kind',
                AnyCodecErrorKind.invalidArgument,
              )
              .having((error) => error.message, 'message', contains('plane 0')),
        ),
      );
    });

    test('an undersized plane buffer cannot be constructed at all', () {
      expect(
        () => VideoFrame(
          pixelFormat: PixelFormat.i420,
          width: 16,
          height: 16,
          planes: <Uint8List>[Uint8List(256), Uint8List(64), Uint8List(64)],
          strides: <int>[16],
        ),
        throwsA(isA<ArgumentError>()),
        reason: 'strides must match the plane count',
      );
      expect(
        () => VideoFrame.i420(
          width: 16,
          height: 16,
          y: Uint8List(8),
          u: Uint8List(64),
          v: Uint8List(64),
        ),
        throwsA(isA<ArgumentError>()),
        reason: 'a 16x16 I420 luma plane needs 256 bytes',
      );
      expect(
        () => AudioFrame(
          sampleFormat: SampleFormat.s16,
          sampleRate: 48000,
          channels: ChannelLayout.stereo,
          frames: 1024,
          data: Uint8List(16),
        ),
        throwsA(isA<ArgumentError>()),
      );
    });
  });

  group('the raw binding refuses a NULL plane instead of reading it', () {
    test(
      'anycodec_encoder_push_video with a NULL plane is invalid_argument',
      () {
        final encoder = abi.anycodec_passthrough_encoder_new(
          abi.ANYCODEC_CODEC_H264,
        );
        expect(encoder, isNot(equals(nullptr)));
        final planes = calloc<Pointer<UnsignedChar>>(3);
        final strides = calloc<UintPtr>(3);
        final buffer = calloc<UnsignedChar>(16);
        final descriptor = calloc<abi.AnyCodecVideoFrame>();
        try {
          strides[0] = 4;
          strides[1] = 2;
          strides[2] = 2;
          planes[0] = buffer;
          planes[1] = nullptr; // the one that must be caught
          planes[2] = buffer;
          descriptor.ref
            ..pixel_format = abi.ANYCODEC_PIXEL_FORMAT_I420
            ..width = 4
            ..height = 4
            ..plane_count = 3
            ..keyframe = 1
            ..pts = 7
            ..planes = planes
            ..strides = strides;

          expect(
            abi.anycodec_encoder_push_video(encoder, descriptor),
            abi.STATUS_INVALID_ARGUMENT,
          );
          expect(lastNativeErrorStatus(), abi.STATUS_INVALID_ARGUMENT);
          expect(lastNativeErrorMessage(), contains('plane 1 is NULL'));

          // The encoder is still usable after the rejected push.
          planes[1] = buffer;
          expect(
            abi.anycodec_encoder_push_video(encoder, descriptor),
            abi.STATUS_OK,
          );
        } finally {
          calloc.free(planes);
          calloc.free(strides);
          calloc.free(buffer);
          calloc.free(descriptor);
          abi.anycodec_encoder_free(encoder);
        }
      },
    );
  });

  group('a production session, through the registry', () {
    test(
      'software_only FLAC encodes for real and refuses the wrong kind',
      () async {
        final capabilities = await AnyCodec.capabilities();
        final flac = capabilities.firstWhere(
          (entry) =>
              entry.codec == Codec.flac &&
              entry.direction == CodecDirection.encode,
        );
        expect(
          flac.hasSoftwareFallback,
          isTrue,
          reason:
              'the built-in FLAC encoder is compiled in and needs no platform '
              'support, so this must hold on every host',
        );

        // `softwareOnly` is what makes this test independent of which platform
        // frameworks the host happens to have: it selects the built-in codec.
        final encoder = await AnyCodec.encoder(
          EncoderConfig.audio(
            codec: Codec.flac,
            sampleRate: 48000,
            bitrate: 128000,
          ),
          preference: BackendPreference.softwareOnly,
        );
        try {
          expect(encoder.path.isSoftware, isTrue);
          expect(encoder.path.backend, Backend.rust);
          expect(encoder.codec, Codec.flac);

          // The Dart layer refuses a picture before the Rust backend has to.
          await expectLater(
            () => encoder.encode(sampleVideoFrame()),
            throwsA(
              isA<AnyCodecException>().having(
                (error) => error.kind,
                'kind',
                AnyCodecErrorKind.invalidArgument,
              ),
            ),
          );

          // A real encode: one FLAC block, then the flush that finishes the stream.
          await encoder.encode(sampleAudioFrame(frames: 4096, pts: 0));
          final packets = await encoder.flush();
          expect(packets, isNotEmpty);
          expect(packets.first.codec, Codec.flac);
          expect(packets.first.length, greaterThan(0));
          expect(
            packets.any((packet) => packet.flags.isHeader),
            isTrue,
            reason: 'the FLAC stream header travels as a header packet',
          );
          final extradata = await encoder.extradata();
          expect(extradata, isNotNull);
          expect(extradata!.length, greaterThan(0));
        } finally {
          await encoder.close();
        }
      },
    );
  });

  group('the pure-Dart diagnostic codec used by the web runtime', () {
    test(
      'round-trips a picture and is byte-compatible with the Rust codec',
      () async {
        final frame = sampleVideoFrame(pts: 777);
        final pure = DiagnosticEncoder(Codec.h264);
        await pure.encode(frame);
        final purePacket = await pure.receive();
        expect(purePacket, isNotNull);

        final decoder = DiagnosticDecoder(Codec.h264);
        await decoder.decode(purePacket!);
        final decoded = await decoder.receive();
        expect(decoded, isA<VideoFrame>());
        final video = decoded! as VideoFrame;
        expect(video.width, frame.width);
        expect(video.pixelFormat, frame.pixelFormat);
        expect(video.pts, frame.pts);
        for (var index = 0; index < frame.planes.length; index++) {
          expect(video.planes[index], orderedEquals(frame.planes[index]));
        }
        expect(pure.path.isSoftware, isTrue);
        expect(pure.path.detail, contains('passthrough'));
        await pure.close();
        await decoder.close();
      },
    );

    test('produces exactly the bytes the native codec produces', () async {
      final frame = sampleVideoFrame(pts: 9);
      final native = await AnyCodec.diagnosticEncoder(Codec.h264);
      final pure = DiagnosticEncoder(Codec.h264);
      try {
        await native.encode(frame.clone());
        await pure.encode(frame.clone());
        final nativePacket = await drainPacket(native);
        final purePacket = await pure.receive();
        expect(nativePacket, isNotNull);
        expect(purePacket, isNotNull);
        expect(
          purePacket!.data,
          orderedEquals(nativePacket!.data),
          reason:
              'the Dart codec must speak the Rust codec wire format, otherwise '
              'a packet could not cross between the web runtime and a native one',
        );
      } finally {
        await native.close();
        await pure.close();
      }
    });
  });
}
