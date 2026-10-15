/// ABI layout tests: struct sizes, field offsets and constant values.
///
/// This is the test that catches an ABI break. `dart:ffi` will happily write a
/// `u32` into the wrong place 鈥?or read eight bytes where Rust wrote four 鈥?and
/// the result is memory corruption a long way from the cause, so the layout the
/// Dart side believes in is pinned here against the numbers
/// `rust/include/anycodec.h` implies for the host.
///
/// ## Host assumptions
///
/// The expected numbers below are for a **64-bit little-endian target with the
/// LP64/LLP64 data model**. On the 32- and 64-bit targets AnyCodec supports
/// (`x86_64`, `aarch64`, `armv7`) every field is either 4 bytes, an 8-byte
/// aligned pointer, or an 8-byte aligned integer, and `char` is 1 byte, so the
/// layout is identical on all of them; the only thing that would change is the
/// size of a pointer, which is why `Pointer` fields are checked by hand below.
///
/// ## Why there is no `offsetOf`/`alignmentOf` call
///
/// `dart:ffi` in Dart 3.13 has neither: they were removed (only `sizeOf<T>()`
/// remains, and it requires a compile-time-constant type argument). Offsets are
/// therefore verified the stronger way 鈥?by writing a sentinel value through the
/// generated field and locating its exact bytes in the raw struct.
library;

import 'dart:ffi';
import 'dart:io';
import 'dart:typed_data';

import 'package:anycodec/anycodec.dart';
import 'package:anycodec/src/bindings/anycodec_bindings_generated.dart' as abi;
import 'package:ffi/ffi.dart';
import 'package:test/test.dart';

/// A distinctive value that cannot occur in zeroed memory.
const int _sentinel32 = 0x11223344;

/// A distinctive 64-bit value for pointer and `i64`/`u64` fields.
const int _sentinel64 = 0x1122334455667788;

void main() {
  group('struct sizes', () {
    test('capability and backend reports', () {
      expect(sizeOf<abi.AnyCodecCapability>(), 24, reason: 'six u32 fields');
      expect(
        sizeOf<abi.AnyCodecBackendInfo>(),
        272,
        reason: 'four u32 fields (16) + char note[256]',
      );
      expect(
        sizeOf<abi.AnyCodecDeviceInfo>(),
        264,
        reason: 'two u32 fields (8) + char[128] + char[64] + char[64]',
      );
      expect(sizeOf<abi.AnyCodecDeviceCodec>(), 28, reason: 'seven u32 fields');
    });

    test('the codec path', () {
      expect(
        sizeOf<abi.AnyCodecCodecPath>(),
        784,
        reason: 'four u32 fields (16) + char detail[256] + char reason[512]',
      );
    });

    test('configurations', () {
      expect(
        sizeOf<abi.AnyCodecEncoderConfig>(),
        88,
        reason:
            'eleven u32 fields (44) + 4 bytes of padding so u64 bitrate lands '
            'at 48 + float quality (56) + preset (60) + option_count (64) + '
            '4 bytes of padding + two pointers (72, 80)',
      );
      expect(
        sizeOf<abi.AnyCodecDecoderConfig>(),
        48,
        reason:
            'eight u32 fields (32) + pointer extradata at 32 + uintptr '
            'extradata_len at 40',
      );
    });

    test('frames and metadata', () {
      expect(
        sizeOf<abi.AnyCodecVideoFrame>(),
        56,
        reason:
            'five u32 fields (20) + 4 bytes padding so i64 pts lands at 24 + '
            'i64 duration (32) + two pointers (40, 48)',
      );
      expect(
        sizeOf<abi.AnyCodecAudioFrame>(),
        40,
        reason:
            'four u32 fields (16) + i64 pts (16) + pointer data (24) + uintptr '
            'len (32)',
      );
      expect(
        sizeOf<abi.AnyCodecVideoFrameInfo>(),
        32,
        reason: 'four u32 fields (16) + i64 pts (16) + i64 duration (24)',
      );
      expect(
        sizeOf<abi.AnyCodecAudioFrameInfo>(),
        24,
        reason: 'four u32 fields (16) + i64 pts (16)',
      );
      expect(sizeOf<abi.AnyCodecVersion>(), 16, reason: 'four u32 fields');
    });
  });

  group('field offsets', () {
    test('AnyCodecCapability', () {
      final pointer = calloc<abi.AnyCodecCapability>();
      try {
        final raw = pointer.cast<Uint8>().asTypedList(
          sizeOf<abi.AnyCodecCapability>(),
        );
        pointer.ref
          ..codec = _sentinel32
          ..kind = _sentinel32 + 1
          ..backends = _sentinel32 + 2
          ..available_backends = _sentinel32 + 3
          ..hardware = _sentinel32 + 4
          ..software = _sentinel32 + 5;
        _expectU32(raw, 0, _sentinel32, 'codec');
        _expectU32(raw, 4, _sentinel32 + 1, 'kind');
        _expectU32(raw, 8, _sentinel32 + 2, 'backends');
        _expectU32(raw, 12, _sentinel32 + 3, 'available_backends');
        _expectU32(raw, 16, _sentinel32 + 4, 'hardware');
        _expectU32(raw, 20, _sentinel32 + 5, 'software');
      } finally {
        calloc.free(pointer);
      }
    });

    test('AnyCodecBackendInfo', () {
      final pointer = calloc<abi.AnyCodecBackendInfo>();
      try {
        final raw = pointer.cast<Uint8>().asTypedList(
          sizeOf<abi.AnyCodecBackendInfo>(),
        );
        pointer.ref
          ..backend = _sentinel32
          ..implementation = _sentinel32 + 1
          ..hardware = _sentinel32 + 2
          ..available = _sentinel32 + 3
          ..note[0] = 0x41
          ..note[1] = 0x42
          ..note[2] = 0;
        _expectU32(raw, 0, _sentinel32, 'backend');
        _expectU32(raw, 4, _sentinel32 + 1, 'implementation');
        _expectU32(raw, 8, _sentinel32 + 2, 'hardware');
        _expectU32(raw, 12, _sentinel32 + 3, 'available');
        expect(raw[16], 0x41, reason: 'note starts at offset 16');
        expect(raw[17], 0x42);
        expect(raw[18], 0);
      } finally {
        calloc.free(pointer);
      }
    });

    test('AnyCodecDeviceInfo and AnyCodecDeviceCodec', () {
      final device = calloc<abi.AnyCodecDeviceInfo>();
      final codec = calloc<abi.AnyCodecDeviceCodec>();
      try {
        final raw = device.cast<Uint8>().asTypedList(
          sizeOf<abi.AnyCodecDeviceInfo>(),
        );
        device.ref
          ..backend = _sentinel32
          ..codec_count = _sentinel32 + 1
          ..name[0] = 0x6E
          ..api_version[0] = 0x76
          ..driver_version[0] = 0x64;
        _expectU32(raw, 0, _sentinel32, 'backend');
        _expectU32(raw, 4, _sentinel32 + 1, 'codec_count');
        expect(raw[8], 0x6E, reason: 'name starts at offset 8');
        expect(raw[136], 0x76, reason: 'api_version starts at offset 136');
        expect(raw[200], 0x64, reason: 'driver_version starts at offset 200');

        final codecRaw = codec.cast<Uint8>().asTypedList(
          sizeOf<abi.AnyCodecDeviceCodec>(),
        );
        codec.ref
          ..backend = _sentinel32
          ..codec = _sentinel32 + 1
          ..decode = _sentinel32 + 2
          ..encode = _sentinel32 + 3
          ..max_width = _sentinel32 + 4
          ..max_height = _sentinel32 + 5
          ..max_bit_depth = _sentinel32 + 6;
        _expectU32(codecRaw, 0, _sentinel32, 'backend');
        _expectU32(codecRaw, 4, _sentinel32 + 1, 'codec');
        _expectU32(codecRaw, 8, _sentinel32 + 2, 'decode');
        _expectU32(codecRaw, 12, _sentinel32 + 3, 'encode');
        _expectU32(codecRaw, 16, _sentinel32 + 4, 'max_width');
        _expectU32(codecRaw, 20, _sentinel32 + 5, 'max_height');
        _expectU32(codecRaw, 24, _sentinel32 + 6, 'max_bit_depth');
      } finally {
        calloc.free(device);
        calloc.free(codec);
      }
    });

    test('AnyCodecCodecPath', () {
      final pointer = calloc<abi.AnyCodecCodecPath>();
      try {
        final raw = pointer.cast<Uint8>().asTypedList(
          sizeOf<abi.AnyCodecCodecPath>(),
        );
        pointer.ref
          ..backend = _sentinel32
          ..implementation = _sentinel32 + 1
          ..hardware_accelerated = _sentinel32 + 2
          ..is_software = _sentinel32 + 3
          ..detail[0] = 0x44
          ..fallback_reason[0] = 0x52;
        _expectU32(raw, 0, _sentinel32, 'backend');
        _expectU32(raw, 4, _sentinel32 + 1, 'implementation');
        _expectU32(raw, 8, _sentinel32 + 2, 'hardware_accelerated');
        _expectU32(raw, 12, _sentinel32 + 3, 'is_software');
        expect(raw[16], 0x44, reason: 'detail starts at offset 16');
        expect(raw[272], 0x52, reason: 'fallback_reason starts at offset 272');
      } finally {
        calloc.free(pointer);
      }
    });

    test('AnyCodecEncoderConfig', () {
      final pointer = calloc<abi.AnyCodecEncoderConfig>();
      try {
        final raw = pointer.cast<Uint8>().asTypedList(
          sizeOf<abi.AnyCodecEncoderConfig>(),
        );
        pointer.ref
          ..codec = _sentinel32
          ..width = _sentinel32 + 1
          ..height = _sentinel32 + 2
          ..pixel_format = _sentinel32 + 3
          ..framerate_num = _sentinel32 + 4
          ..framerate_den = _sentinel32 + 5
          ..keyframe_interval = _sentinel32 + 6
          ..sample_rate = _sentinel32 + 7
          ..channels = _sentinel32 + 8
          ..sample_format = _sentinel32 + 9
          ..frame_duration_ms = _sentinel32 + 10
          ..bitrate = _sentinel64
          ..quality = 0.5
          ..preset = _sentinel32 + 11
          ..option_count = _sentinel32 + 12
          ..option_keys = Pointer.fromAddress(_sentinel64)
          ..option_values = Pointer.fromAddress(_sentinel64 + 8);
        _expectU32(raw, 0, _sentinel32, 'codec');
        _expectU32(raw, 4, _sentinel32 + 1, 'width');
        _expectU32(raw, 8, _sentinel32 + 2, 'height');
        _expectU32(raw, 12, _sentinel32 + 3, 'pixel_format');
        _expectU32(raw, 16, _sentinel32 + 4, 'framerate_num');
        _expectU32(raw, 20, _sentinel32 + 5, 'framerate_den');
        _expectU32(raw, 24, _sentinel32 + 6, 'keyframe_interval');
        _expectU32(raw, 28, _sentinel32 + 7, 'sample_rate');
        _expectU32(raw, 32, _sentinel32 + 8, 'channels');
        _expectU32(raw, 36, _sentinel32 + 9, 'sample_format');
        _expectU32(raw, 40, _sentinel32 + 10, 'frame_duration_ms');
        _expectU64(raw, 48, _sentinel64, 'bitrate');
        expect(
          ByteData.sublistView(raw).getFloat32(56, Endian.little),
          closeTo(0.5, 1e-6),
          reason: 'quality is a C float at offset 56',
        );
        _expectU32(raw, 60, _sentinel32 + 11, 'preset');
        _expectU32(raw, 64, _sentinel32 + 12, 'option_count');
        _expectU64(raw, 72, _sentinel64, 'option_keys');
        _expectU64(raw, 80, _sentinel64 + 8, 'option_values');
      } finally {
        calloc.free(pointer);
      }
    });

    test('AnyCodecDecoderConfig', () {
      final pointer = calloc<abi.AnyCodecDecoderConfig>();
      try {
        final raw = pointer.cast<Uint8>().asTypedList(
          sizeOf<abi.AnyCodecDecoderConfig>(),
        );
        pointer.ref
          ..codec = _sentinel32
          ..width = _sentinel32 + 1
          ..height = _sentinel32 + 2
          ..pixel_format = _sentinel32 + 3
          ..sample_rate = _sentinel32 + 4
          ..channels = _sentinel32 + 5
          ..sample_format = _sentinel32 + 6
          ..annex_b = _sentinel32 + 7
          ..extradata = Pointer.fromAddress(_sentinel64)
          ..extradata_len = _sentinel64 + 8;
        _expectU32(raw, 0, _sentinel32, 'codec');
        _expectU32(raw, 4, _sentinel32 + 1, 'width');
        _expectU32(raw, 8, _sentinel32 + 2, 'height');
        _expectU32(raw, 12, _sentinel32 + 3, 'pixel_format');
        _expectU32(raw, 16, _sentinel32 + 4, 'sample_rate');
        _expectU32(raw, 20, _sentinel32 + 5, 'channels');
        _expectU32(raw, 24, _sentinel32 + 6, 'sample_format');
        _expectU32(raw, 28, _sentinel32 + 7, 'annex_b');
        _expectU64(raw, 32, _sentinel64, 'extradata');
        _expectU64(raw, 40, _sentinel64 + 8, 'extradata_len');
      } finally {
        calloc.free(pointer);
      }
    });

    test('AnyCodecVideoFrame', () {
      final pointer = calloc<abi.AnyCodecVideoFrame>();
      try {
        final raw = pointer.cast<Uint8>().asTypedList(
          sizeOf<abi.AnyCodecVideoFrame>(),
        );
        pointer.ref
          ..pixel_format = _sentinel32
          ..width = _sentinel32 + 1
          ..height = _sentinel32 + 2
          ..plane_count = _sentinel32 + 3
          ..keyframe = _sentinel32 + 4
          ..pts = _sentinel64
          ..duration = _sentinel64 + 1
          ..planes = Pointer.fromAddress(_sentinel64 + 2)
          ..strides = Pointer.fromAddress(_sentinel64 + 3);
        _expectU32(raw, 0, _sentinel32, 'pixel_format');
        _expectU32(raw, 4, _sentinel32 + 1, 'width');
        _expectU32(raw, 8, _sentinel32 + 2, 'height');
        _expectU32(raw, 12, _sentinel32 + 3, 'plane_count');
        _expectU32(raw, 16, _sentinel32 + 4, 'keyframe');
        _expectU64(raw, 24, _sentinel64, 'pts');
        _expectU64(raw, 32, _sentinel64 + 1, 'duration');
        _expectU64(raw, 40, _sentinel64 + 2, 'planes');
        _expectU64(raw, 48, _sentinel64 + 3, 'strides');
      } finally {
        calloc.free(pointer);
      }
    });

    test(
      'AnyCodecAudioFrame, AnyCodecVideoFrameInfo, AnyCodecAudioFrameInfo',
      () {
        final audio = calloc<abi.AnyCodecAudioFrame>();
        final videoInfo = calloc<abi.AnyCodecVideoFrameInfo>();
        final audioInfo = calloc<abi.AnyCodecAudioFrameInfo>();
        try {
          final rawAudio = audio.cast<Uint8>().asTypedList(
            sizeOf<abi.AnyCodecAudioFrame>(),
          );
          audio.ref
            ..sample_format = _sentinel32
            ..sample_rate = _sentinel32 + 1
            ..channels = _sentinel32 + 2
            ..frames = _sentinel32 + 3
            ..pts = _sentinel64
            ..data = Pointer.fromAddress(_sentinel64 + 1)
            ..len = _sentinel64 + 2;
          _expectU32(rawAudio, 0, _sentinel32, 'sample_format');
          _expectU32(rawAudio, 4, _sentinel32 + 1, 'sample_rate');
          _expectU32(rawAudio, 8, _sentinel32 + 2, 'channels');
          _expectU32(rawAudio, 12, _sentinel32 + 3, 'frames');
          _expectU64(rawAudio, 16, _sentinel64, 'pts');
          _expectU64(rawAudio, 24, _sentinel64 + 1, 'data');
          _expectU64(rawAudio, 32, _sentinel64 + 2, 'len');

          final rawVideoInfo = videoInfo.cast<Uint8>().asTypedList(
            sizeOf<abi.AnyCodecVideoFrameInfo>(),
          );
          videoInfo.ref
            ..pixel_format = _sentinel32
            ..width = _sentinel32 + 1
            ..height = _sentinel32 + 2
            ..plane_count = _sentinel32 + 3
            ..pts = _sentinel64
            ..duration = _sentinel64 + 1;
          _expectU32(rawVideoInfo, 0, _sentinel32, 'pixel_format');
          _expectU32(rawVideoInfo, 4, _sentinel32 + 1, 'width');
          _expectU32(rawVideoInfo, 8, _sentinel32 + 2, 'height');
          _expectU32(rawVideoInfo, 12, _sentinel32 + 3, 'plane_count');
          _expectU64(rawVideoInfo, 16, _sentinel64, 'pts');
          _expectU64(rawVideoInfo, 24, _sentinel64 + 1, 'duration');

          final rawAudioInfo = audioInfo.cast<Uint8>().asTypedList(
            sizeOf<abi.AnyCodecAudioFrameInfo>(),
          );
          audioInfo.ref
            ..sample_format = _sentinel32
            ..sample_rate = _sentinel32 + 1
            ..channels = _sentinel32 + 2
            ..frames = _sentinel32 + 3
            ..pts = _sentinel64;
          _expectU32(rawAudioInfo, 0, _sentinel32, 'sample_format');
          _expectU32(rawAudioInfo, 4, _sentinel32 + 1, 'sample_rate');
          _expectU32(rawAudioInfo, 8, _sentinel32 + 2, 'channels');
          _expectU32(rawAudioInfo, 12, _sentinel32 + 3, 'frames');
          _expectU64(rawAudioInfo, 16, _sentinel64, 'pts');
        } finally {
          calloc.free(audio);
          calloc.free(videoInfo);
          calloc.free(audioInfo);
        }
      },
    );

    test('AnyCodecVersion', () {
      final pointer = calloc<abi.AnyCodecVersion>();
      try {
        final raw = pointer.cast<Uint8>().asTypedList(
          sizeOf<abi.AnyCodecVersion>(),
        );
        pointer.ref
          ..abi = _sentinel32
          ..major = _sentinel32 + 1
          ..minor = _sentinel32 + 2
          ..patch = _sentinel32 + 3;
        _expectU32(raw, 0, _sentinel32, 'abi');
        _expectU32(raw, 4, _sentinel32 + 1, 'major');
        _expectU32(raw, 8, _sentinel32 + 2, 'minor');
        _expectU32(raw, 12, _sentinel32 + 3, 'patch');
      } finally {
        calloc.free(pointer);
      }
    });
  });

  group('constants match the generated bindings', () {
    test('codecs', () {
      expect(abi.ANYCODEC_CODEC_H264, Codec.h264.raw);
      expect(abi.ANYCODEC_CODEC_H265, Codec.h265.raw);
      expect(abi.ANYCODEC_CODEC_H266, Codec.h266.raw);
      expect(abi.ANYCODEC_CODEC_VP8, Codec.vp8.raw);
      expect(abi.ANYCODEC_CODEC_VP9, Codec.vp9.raw);
      expect(abi.ANYCODEC_CODEC_AV1, Codec.av1.raw);
      expect(abi.ANYCODEC_CODEC_MP3, Codec.mp3.raw);
      expect(abi.ANYCODEC_CODEC_AAC, Codec.aac.raw);
      expect(abi.ANYCODEC_CODEC_VORBIS, Codec.vorbis.raw);
      expect(abi.ANYCODEC_CODEC_OPUS, Codec.opus.raw);
      expect(abi.ANYCODEC_CODEC_FLAC, Codec.flac.raw);
      expect(abi.ANYCODEC_CODEC_ALAC, Codec.alac.raw);
      for (final codec in Codec.values) {
        expect(Codec.fromRaw(codec.raw), codec);
      }
    });

    test('pixel formats, sample formats and channel layouts', () {
      expect(abi.ANYCODEC_PIXEL_FORMAT_I420, PixelFormat.i420.raw);
      expect(abi.ANYCODEC_PIXEL_FORMAT_NV12, PixelFormat.nv12.raw);
      expect(abi.ANYCODEC_PIXEL_FORMAT_NV21, PixelFormat.nv21.raw);
      expect(abi.ANYCODEC_PIXEL_FORMAT_RGBA8, PixelFormat.rgba8.raw);
      expect(abi.ANYCODEC_PIXEL_FORMAT_BGRA8, PixelFormat.bgra8.raw);
      expect(abi.ANYCODEC_PIXEL_FORMAT_YUY2, PixelFormat.yuy2.raw);
      expect(abi.ANYCODEC_PIXEL_FORMAT_P010, PixelFormat.p010.raw);

      expect(abi.ANYCODEC_SAMPLE_FORMAT_F32, SampleFormat.f32.raw);
      expect(abi.ANYCODEC_SAMPLE_FORMAT_S16, SampleFormat.s16.raw);
      expect(abi.ANYCODEC_SAMPLE_FORMAT_S32, SampleFormat.s32.raw);
      expect(abi.ANYCODEC_SAMPLE_FORMAT_U8, SampleFormat.u8.raw);

      expect(abi.ANYCODEC_CHANNEL_LAYOUT_MONO, ChannelLayout.mono.raw);
      expect(abi.ANYCODEC_CHANNEL_LAYOUT_STEREO, ChannelLayout.stereo.raw);
      expect(abi.ANYCODEC_CHANNEL_LAYOUT_STEREO_21, ChannelLayout.stereo21.raw);
      expect(abi.ANYCODEC_CHANNEL_LAYOUT_QUAD, ChannelLayout.quad.raw);
      expect(abi.ANYCODEC_CHANNEL_LAYOUT_FIVE_ONE, ChannelLayout.fiveOne.raw);
      expect(abi.ANYCODEC_CHANNEL_LAYOUT_SEVEN_ONE, ChannelLayout.sevenOne.raw);
    });

    test('backends, implementations, preferences and presets', () {
      expect(abi.ANYCODEC_BACKEND_MEDIACODEC, Backend.mediaCodec.raw);
      expect(abi.ANYCODEC_BACKEND_MEDIAFOUNDATION, Backend.mediaFoundation.raw);
      expect(abi.ANYCODEC_BACKEND_VIDEOTOOLBOX, Backend.videoToolbox.raw);
      expect(abi.ANYCODEC_BACKEND_AUDIOTOOLBOX, Backend.audioToolbox.raw);
      expect(abi.ANYCODEC_BACKEND_GSTREAMER, Backend.gStreamer.raw);
      expect(abi.ANYCODEC_BACKEND_WEBCODECS, Backend.webCodecs.raw);
      expect(abi.ANYCODEC_BACKEND_RUST, Backend.rust.raw);
      expect(abi.ANYCODEC_BACKEND_FFMPEG, Backend.ffmpeg.raw);
      expect(abi.ANYCODEC_BACKEND_EXTERNAL, Backend.external.raw);

      expect(abi.ANYCODEC_IMPLEMENTATION_NATIVE, Implementation.native.raw);
      expect(abi.ANYCODEC_IMPLEMENTATION_SOFTWARE, Implementation.software.raw);

      expect(abi.ANYCODEC_PREFERENCE_AUTO, BackendPreference.auto.raw);
      expect(
        abi.ANYCODEC_PREFERENCE_NATIVE_ONLY,
        BackendPreference.nativeOnly.raw,
      );
      expect(
        abi.ANYCODEC_PREFERENCE_SOFTWARE_ONLY,
        BackendPreference.softwareOnly.raw,
      );
      expect(
        abi.ANYCODEC_PREFERENCE_REQUIRE_HARDWARE,
        BackendPreference.requireHardware.raw,
      );

      expect(abi.ANYCODEC_PRESET_SPEED, Preset.speed.raw);
      expect(abi.ANYCODEC_PRESET_BALANCED, Preset.balanced.raw);
      expect(abi.ANYCODEC_PRESET_QUALITY, Preset.quality.raw);
    });

    test('packet flags and capacities', () {
      expect(abi.ANYCODEC_PACKET_FLAG_KEYFRAME, PacketFlags.keyframe.bits);
      expect(abi.ANYCODEC_PACKET_FLAG_HEADER, PacketFlags.header.bits);
      expect(abi.ANYCODEC_PACKET_FLAG_EOS, PacketFlags.eos.bits);
      expect(abi.ANYCODEC_PACKET_FLAG_HARDWARE, PacketFlags.hardware.bits);
      expect(abi.ANYCODEC_PACKET_FLAG_CORRUPT, PacketFlags.corrupt.bits);

      expect(abi.ANYCODEC_NOTE_CAPACITY, 256);
      expect(abi.ANYCODEC_DETAIL_CAPACITY, 256);
      expect(abi.ANYCODEC_REASON_CAPACITY, 512);
      expect(abi.ANYCODEC_DEVICE_NAME_CAPACITY, 128);
      expect(abi.ANYCODEC_VERSION_CAPACITY, 64);
      expect(abi.ANYCODEC_STRING_CAPACITY, 512);
    });

    test('the header exports no short aliases for internal constants', () {
      // Regression guard. cbindgen emits *every* `pub const` in the Rust crate
      // and honours neither `include` nor `exclude` for constants, so an
      // internal constant that is not `pub(crate)` shows up here as a bare name
      // next to its `ANYCODEC_`-prefixed twin. A consumer that used the bare
      // name would depend on something that is not part of the ABI.
      //
      // The Rust side guards this from the other direction, in
      // `ffi::header_tests::the_header_defines_only_abi_symbols`.
      for (final name in _allDefinedSymbols()) {
        expect(
          name == 'ANYCODEC_H' ||
              name.startsWith('ANYCODEC_') ||
              name.startsWith('STATUS_'),
          isTrue,
          reason: '`$name` is not in the ANYCODEC_/STATUS_ namespace',
        );
      }
    });
    test('status codes match the Dart error kinds', () {
      expect(abi.ANYCODEC_ABI_VERSION, 1);
      expect(abi.STATUS_OK, 0);
      expect(abi.STATUS_GOT_OUTPUT, 1);
      expect(abi.STATUS_NEED_MORE_INPUT, 0);
      expect(
        abi.STATUS_INVALID_ARGUMENT,
        AnyCodecErrorKind.invalidArgument.status,
      );
      expect(abi.STATUS_UNSUPPORTED, AnyCodecErrorKind.unsupported.status);
      expect(abi.STATUS_UNAVAILABLE, AnyCodecErrorKind.unavailable.status);
      expect(abi.STATUS_BACKEND, AnyCodecErrorKind.backend.status);
      expect(abi.STATUS_STATE, AnyCodecErrorKind.state.status);
      expect(abi.STATUS_OUT_OF_MEMORY, AnyCodecErrorKind.outOfMemory.status);
      expect(abi.STATUS_IO, AnyCodecErrorKind.io.status);
      expect(abi.STATUS_INTERNAL, AnyCodecErrorKind.internal.status);
      expect(
        abi.ANYCODEC_PASSTHROUGH_INVALID_CODEC,
        abi.STATUS_INVALID_ARGUMENT,
      );
      for (final kind in AnyCodecErrorKind.values) {
        if (kind == AnyCodecErrorKind.unknown) {
          continue;
        }
        expect(AnyCodecErrorKind.fromStatus(kind.status), kind);
      }
    });

    test('every codec in the enum has a distinct ABI id', () {
      final ids = <int>{for (final codec in Codec.values) codec.raw};
      expect(ids.length, Codec.values.length);
      expect(Codec.values.length, 12, reason: '12 codecs x 2 directions = 24');
      for (final format in PixelFormat.values) {
        expect(PixelFormat.fromRaw(format.raw), format);
      }
      for (final layout in ChannelLayout.values) {
        expect(ChannelLayout.fromRaw(layout.raw), layout);
      }
      for (final format in SampleFormat.values) {
        expect(SampleFormat.fromRaw(format.raw), format);
      }
      for (final backend in Backend.values) {
        expect(Backend.fromRaw(backend.raw), backend);
      }
    });
  });
}

void _expectU32(Uint8List raw, int offset, int expected, String field) {
  expect(
    ByteData.sublistView(raw).getUint32(offset, Endian.little),
    expected & 0xffffffff,
    reason: '$field should live at byte offset $offset',
  );
}

void _expectU64(Uint8List raw, int offset, int expected, String field) {
  expect(
    ByteData.sublistView(raw).getUint64(offset, Endian.little),
    expected & 0xffffffffffffffff,
    reason: '$field should live at byte offset $offset',
  );
}

/// Every `#define` name in the generated C header.
///
/// The header is the ABI contract; reading it here is what lets the test above
/// assert that nothing outside the `ANYCODEC_`/`STATUS_` namespaces leaked in.
List<String> _allDefinedSymbols() {
  final header = File('rust/include/anycodec.h');
  expect(
    header.existsSync(),
    isTrue,
    reason:
        'regenerate it with: cd rust && cbindgen --config cbindgen.toml '
        '--crate anycodec --output include/anycodec.h',
  );
  return header
      .readAsLinesSync()
      .where((line) => line.startsWith('#define '))
      .map(
        (line) => line.substring('#define '.length).split(RegExp(r'\s+')).first,
      )
      .toList();
}
