/// ABI-level binding tests: is every declared symbol actually reachable?
///
/// The point of these tests is that the native asset
/// (`@ffi.DefaultAsset('package:anycodec/src/bindings/anycodec_bindings_generated.dart')`,
/// produced by `hook/build.dart`) resolves at all, that the library and the
/// Dart bindings agree on the ABI revision, and that calling every exported
/// function with the most hostile arguments available from Dart 鈥?NULL pointers
/// and zero 鈥?produces a documented result instead of a crash.
///
/// A missing symbol would fail at the *first* call in this file, because
/// `dart:ffi` resolves `@Native` externals lazily and throws
/// `ArgumentError` for an unknown symbol.
library;

import 'dart:ffi';

import 'package:anycodec/src/bindings/anycodec_bindings_generated.dart' as abi;
import 'package:anycodec/src/native/status.dart';
import 'package:ffi/ffi.dart';
import 'package:test/test.dart';

void main() {
  test('the native asset resolves and reports the ABI revision', () {
    // The whole point of the generated `@Native` externals: if the hook had not
    // built the cdylib, or had named the asset differently, this line would
    // throw rather than return a number.
    expect(abi.anycodec_abi_version(), abi.ANYCODEC_ABI_VERSION);
    expect(
      abi.ANYCODEC_ABI_VERSION,
      1,
      reason: 'the frozen C ABI is revision 1',
    );
  });

  test('anycodec_version writes a plausible semantic version', () {
    final pointer = calloc<abi.AnyCodecVersion>();
    try {
      expect(abi.anycodec_version(pointer), abi.STATUS_OK);
      final version = pointer.ref;
      expect(version.abi, abi.ANYCODEC_ABI_VERSION);
      expect(version.major, greaterThanOrEqualTo(0));
      expect(version.minor, greaterThanOrEqualTo(0));
      expect(version.patch, greaterThanOrEqualTo(0));
      // The crate is 0.0.1 today; a future release must at least stay parseable.
      expect(version.major, lessThan(1000));
      expect(version.minor, lessThan(1000));
      expect(version.patch, lessThan(1000));
    } finally {
      calloc.free(pointer);
    }
    expect(
      abi.anycodec_version_string(),
      isNot(equals(nullptr)),
      reason: 'the version string is a static C string, never NULL',
    );
  });

  group('every exported symbol is callable with NULL/zero arguments', () {
    setUp(clearNativeError);

    test('version and diagnostics', () {
      expect(abi.anycodec_abi_version(), abi.ANYCODEC_ABI_VERSION);
      expect(abi.anycodec_version(nullptr), abi.STATUS_INVALID_ARGUMENT);
      expect(abi.anycodec_version_string(), isNot(equals(nullptr)));

      clearNativeError();
      expect(abi.anycodec_last_error_status(), abi.STATUS_OK);
      expect(abi.anycodec_last_error_message(), equals(nullptr));
      abi.anycodec_clear_error();
      expect(abi.anycodec_last_error_status(), abi.STATUS_OK);
    });

    test('capabilities', () {
      expect(abi.anycodec_capability_count(), 24);
      // The documented sizing protocol: NULL with capacity 0 returns the total.
      expect(
        abi.anycodec_capabilities(nullptr, 0),
        abi.anycodec_capability_count(),
      );
      expect(abi.anycodec_capability_backends(0xdead, 0, nullptr, 0), 0);
      expect(abi.anycodec_capability_summary(), isNot(equals(nullptr)));
    });

    test('device list', () {
      expect(abi.anycodec_device_list_len(nullptr), 0);
      abi.anycodec_device_list_free(nullptr);
      expect(
        abi.anycodec_device_list_device(nullptr, 0, nullptr),
        abi.STATUS_INVALID_ARGUMENT,
      );
      expect(
        abi.anycodec_device_list_codec(nullptr, 0, 0, nullptr),
        abi.STATUS_INVALID_ARGUMENT,
      );

      final list = abi.anycodec_probe_devices();
      expect(
        list,
        isNot(equals(nullptr)),
        reason: 'an empty list is still a handle',
      );
      expect(abi.anycodec_device_list_len(list), lessThan(1024));
      abi.anycodec_device_list_free(list);
    });

    test('encoders', () {
      expect(abi.anycodec_encoder_new(nullptr, 0), equals(nullptr));
      expect(abi.anycodec_encoder_codec(nullptr), 0);
      expect(
        abi.anycodec_encoder_path(nullptr, nullptr),
        abi.STATUS_INVALID_ARGUMENT,
      );
      expect(
        abi.anycodec_encoder_push_video(nullptr, nullptr),
        abi.STATUS_INVALID_ARGUMENT,
      );
      expect(
        abi.anycodec_encoder_push_audio(nullptr, nullptr),
        abi.STATUS_INVALID_ARGUMENT,
      );
      expect(
        abi.anycodec_encoder_receive(nullptr, nullptr),
        abi.STATUS_INVALID_ARGUMENT,
      );
      expect(abi.anycodec_encoder_flush(nullptr), abi.STATUS_INVALID_ARGUMENT);
      expect(
        abi.anycodec_encoder_set_bitrate(nullptr, 0),
        abi.STATUS_INVALID_ARGUMENT,
      );
      expect(
        abi.anycodec_encoder_request_keyframe(nullptr),
        abi.STATUS_INVALID_ARGUMENT,
      );
      expect(
        abi.anycodec_encoder_extradata(nullptr, nullptr),
        abi.STATUS_INVALID_ARGUMENT,
      );
      abi.anycodec_encoder_free(nullptr);
    });

    test('decoders', () {
      expect(abi.anycodec_decoder_new(nullptr, 0), equals(nullptr));
      expect(abi.anycodec_decoder_codec(nullptr), 0);
      expect(
        abi.anycodec_decoder_path(nullptr, nullptr),
        abi.STATUS_INVALID_ARGUMENT,
      );
      expect(
        abi.anycodec_decoder_push(nullptr, nullptr),
        abi.STATUS_INVALID_ARGUMENT,
      );
      expect(
        abi.anycodec_decoder_push_bytes(nullptr, nullptr, 0, 0, 0),
        abi.STATUS_INVALID_ARGUMENT,
      );
      expect(
        abi.anycodec_decoder_receive(nullptr, nullptr),
        abi.STATUS_INVALID_ARGUMENT,
      );
      expect(abi.anycodec_decoder_flush(nullptr), abi.STATUS_INVALID_ARGUMENT);
      abi.anycodec_decoder_free(nullptr);
    });

    test('packets', () {
      // Codec 0 is not a codec, so the constructor refuses before it touches
      // `data`.
      expect(
        abi.anycodec_packet_new(0, nullptr, 0, 0, 0, 0, 0),
        equals(nullptr),
      );
      expect(abi.anycodec_packet_codec(nullptr), 0);
      expect(abi.anycodec_packet_data(nullptr), equals(nullptr));
      expect(abi.anycodec_packet_len(nullptr), 0);
      expect(abi.anycodec_packet_pts(nullptr), 0);
      expect(abi.anycodec_packet_dts(nullptr), 0);
      expect(abi.anycodec_packet_duration(nullptr), 0);
      expect(abi.anycodec_packet_flags(nullptr), 0);
      abi.anycodec_packet_free(nullptr);
    });

    test('frames and buffers', () {
      expect(abi.anycodec_frame_is_video(nullptr), 0);
      expect(abi.anycodec_frame_pts(nullptr), 0);
      expect(
        abi.anycodec_frame_video_info(nullptr, nullptr),
        abi.STATUS_INVALID_ARGUMENT,
      );
      expect(abi.anycodec_frame_video_plane(nullptr, 0), equals(nullptr));
      expect(abi.anycodec_frame_video_plane_len(nullptr, 0), 0);
      expect(abi.anycodec_frame_video_stride(nullptr, 0), 0);
      expect(
        abi.anycodec_frame_audio_info(nullptr, nullptr),
        abi.STATUS_INVALID_ARGUMENT,
      );
      expect(abi.anycodec_frame_audio_data(nullptr), equals(nullptr));
      expect(abi.anycodec_frame_audio_len(nullptr), 0);
      abi.anycodec_frame_free(nullptr);

      expect(abi.anycodec_buffer_data(nullptr), equals(nullptr));
      expect(abi.anycodec_buffer_len(nullptr), 0);
      abi.anycodec_buffer_free(nullptr);
    });

    test('the diagnostic codec entry points', () {
      expect(abi.anycodec_passthrough_codec_id(), greaterThan(0));
      expect(abi.anycodec_passthrough_encoder_new(0), equals(nullptr));
      expect(abi.anycodec_passthrough_decoder_new(0), equals(nullptr));
    });
  });
}
