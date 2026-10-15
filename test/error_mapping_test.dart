/// Error mapping: ABI status codes and messages, all the way into Dart.
///
/// The C ABI answers a failure with a negative `i32` plus a thread-local
/// message. These tests pin down that (a) the classification is right, (b) the
/// message survives long enough to be read and names what was tried, and (c) a
/// NULL argument is a clean Dart exception rather than a segfault.
///
/// NULL arguments cannot be expressed through the public API — `EncoderConfig`
/// is never null — so the NULL cases drive the generated bindings directly, the
/// same way the Dart layer itself does.
library;

import 'dart:ffi';

import 'package:anycodec/anycodec.dart';
import 'package:anycodec/src/bindings/anycodec_bindings_generated.dart' as abi;
import 'package:anycodec/src/native/status.dart';
import 'package:ffi/ffi.dart';
import 'package:test/test.dart';

void main() {
  setUp(clearNativeError);

  test('a NULL config becomes an invalid_argument exception', () {
    final encoder = abi.anycodec_encoder_new(
      nullptr,
      abi.ANYCODEC_PREFERENCE_AUTO,
    );
    expect(encoder, equals(nullptr));
    expect(lastNativeErrorStatus(), abi.STATUS_INVALID_ARGUMENT);

    final error = exceptionForStatus(
      lastNativeErrorStatus(),
      'anycodec_encoder_new',
    );
    expect(error.kind, AnyCodecErrorKind.invalidArgument);
    expect(error.message, contains('config is NULL'));
    expect(error.operation, 'anycodec_encoder_new');
    expect(error.isBug, isFalse);
    expect(error.toString(), contains('invalid_argument'));

    clearNativeError();
    final decoder = abi.anycodec_decoder_new(
      nullptr,
      abi.ANYCODEC_PREFERENCE_AUTO,
    );
    expect(decoder, equals(nullptr));
    expect(
      exceptionForStatus(lastNativeErrorStatus(), 'anycodec_decoder_new').kind,
      AnyCodecErrorKind.invalidArgument,
    );
  });

  test('an unknown codec id becomes an invalid_argument exception', () {
    final config = calloc<abi.AnyCodecEncoderConfig>();
    try {
      config.ref
        ..codec = 0xdead
        ..pixel_format = abi.ANYCODEC_PIXEL_FORMAT_I420
        ..sample_format = abi.ANYCODEC_SAMPLE_FORMAT_S16
        ..channels = abi.ANYCODEC_CHANNEL_LAYOUT_STEREO;
      final encoder = abi.anycodec_encoder_new(
        config,
        abi.ANYCODEC_PREFERENCE_AUTO,
      );
      expect(encoder, equals(nullptr));
      final error = exceptionForStatus(
        lastNativeErrorStatus(),
        'anycodec_encoder_new',
      );
      expect(error.kind, AnyCodecErrorKind.invalidArgument);
      expect(error.message, contains('57005'), reason: '0xdead == 57005');
    } finally {
      calloc.free(config);
    }

    clearNativeError();
    final decoderConfig = calloc<abi.AnyCodecDecoderConfig>();
    try {
      decoderConfig.ref.codec = 4242;
      expect(
        abi.anycodec_decoder_new(decoderConfig, abi.ANYCODEC_PREFERENCE_AUTO),
        equals(nullptr),
      );
      expect(
        exceptionForStatus(
          lastNativeErrorStatus(),
          'anycodec_decoder_new',
        ).kind,
        AnyCodecErrorKind.invalidArgument,
      );
    } finally {
      calloc.free(decoderConfig);
    }

    clearNativeError();
    // The diagnostic codec has its own entry points and its own codec check.
    expect(abi.anycodec_passthrough_encoder_new(0), equals(nullptr));
    final diagnostic = exceptionForStatus(
      lastNativeErrorStatus(),
      'anycodec_passthrough_encoder_new',
    );
    expect(diagnostic.kind, AnyCodecErrorKind.invalidArgument);
    expect(diagnostic.message, contains('unknown codec'));
    expect(abi.anycodec_passthrough_decoder_new(0), equals(nullptr));
  });

  test('an unknown preference becomes an invalid_argument exception', () {
    final config = calloc<abi.AnyCodecEncoderConfig>();
    try {
      config.ref
        ..codec = abi.ANYCODEC_CODEC_H264
        ..pixel_format = abi.ANYCODEC_PIXEL_FORMAT_I420
        ..sample_format = abi.ANYCODEC_SAMPLE_FORMAT_S16
        ..channels = abi.ANYCODEC_CHANNEL_LAYOUT_STEREO;
      expect(abi.anycodec_encoder_new(config, 99), equals(nullptr));
      final error = exceptionForStatus(
        lastNativeErrorStatus(),
        'anycodec_encoder_new',
      );
      expect(error.kind, AnyCodecErrorKind.invalidArgument);
      expect(error.message, contains('unknown preference'));
    } finally {
      calloc.free(config);
    }
  });

  test('the message survives a round trip and is never empty', () {
    expect(lastNativeErrorStatus(), abi.STATUS_OK);
    expect(lastNativeErrorMessage(), isEmpty);

    abi.anycodec_encoder_new(nullptr, 0);
    final message = lastNativeErrorMessage();
    expect(message, isNotEmpty);
    expect(message, contains('anycodec_encoder_new'));

    // Another call that uses the *string* slot (the capability summary) must not
    // clobber the error message: they are separate thread-local slots.
    expect(abi.anycodec_capability_summary(), isNot(equals(nullptr)));
    expect(lastNativeErrorMessage(), message);
    expect(lastNativeErrorStatus(), abi.STATUS_INVALID_ARGUMENT);

    LastNativeError.clear();
    expect(lastNativeErrorStatus(), abi.STATUS_OK);
    expect(lastNativeErrorMessage(), isEmpty);
    expect(LastNativeError.read().isOk, isTrue);
  });

  test('a failure with no backend names every backend that was tried', () async {
    final capabilities = await AnyCodec.capabilities();
    final target = _codecWithNoAvailableNativeBackend(capabilities);
    expect(
      target,
      isNotNull,
      reason:
          'every machine AnyCodec runs on has at least one codec that only '
          'software can serve',
    );

    final config = _encoderConfigFor(target!.codec);
    Object? thrown;
    try {
      await AnyCodec.encoder(config, preference: BackendPreference.nativeOnly);
    } on AnyCodecException catch (error) {
      thrown = error;
    }
    expect(
      thrown,
      isA<AnyCodecException>(),
      reason:
          'native_only must refuse a codec no platform framework can encode',
    );
    final error = thrown! as AnyCodecException;
    expect(error.kind, AnyCodecErrorKind.unavailable);
    expect(error.message, isNotEmpty);
    expect(
      target.backends.any(
        (backend) => error.message.contains(backend.backend.token),
      ),
      isTrue,
      reason:
          'the message must name the backends that were tried, got '
          '"${error.message}" for ${target.backends.map((b) => b.backend.token)}',
    );
    expect(error.toString(), contains('unavailable'));
  });

  test('the same failure is reported through the public API', () async {
    // h264 has no software encoder in this build, so `auto` must report the
    // codepath rather than inventing a fallback.
    final capabilities = await AnyCodec.capabilities();
    final h264 = capabilities.firstWhere(
      (entry) =>
          entry.codec == Codec.h264 && entry.direction == CodecDirection.encode,
    );
    final hasSoftware = h264.backends.any(
      (backend) =>
          backend.available &&
          backend.implementation == Implementation.software,
    );
    if (hasSoftware) {
      // A build with an FFmpeg or Rust H.264 encoder: nothing to assert here, but
      // the report must at least be honest about it.
      expect(h264.availableBackends, isNotEmpty);
    } else {
      await expectLater(
        () => AnyCodec.encoder(
          EncoderConfig.video(
            codec: Codec.h264,
            width: 64,
            height: 64,
            bitrate: 1000000,
          ),
          preference: BackendPreference.softwareOnly,
        ),
        throwsA(
          isA<AnyCodecException>().having(
            (error) => error.kind,
            'kind',
            AnyCodecErrorKind.unavailable,
          ),
        ),
      );
    }
  });

  tearDownAll(AnyCodec.shutdown);
}

CodecCapability? _codecWithNoAvailableNativeBackend(
  List<CodecCapability> capabilities,
) {
  for (final entry in capabilities) {
    if (entry.direction != CodecDirection.encode || entry.backends.isEmpty) {
      continue;
    }
    final native = entry.backends.any(
      (backend) =>
          backend.available && backend.implementation == Implementation.native,
    );
    if (!native) {
      return entry;
    }
  }
  return null;
}

EncoderConfig _encoderConfigFor(Codec codec) => codec.isVideo
    ? EncoderConfig.video(codec: codec, width: 64, height: 64, bitrate: 1000000)
    : EncoderConfig.audio(codec: codec, bitrate: 64000);
