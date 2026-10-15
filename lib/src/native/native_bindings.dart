/// The typed facade over the generated `@ffi.Native` bindings.
///
/// Everything in this library runs on a **worker isolate**, never on the UI
/// isolate: it is here, and only here, that native handles are created, used,
/// released and converted to and from the memory-safe model layer. The public
/// API (`package:anycodec/anycodec.dart`) never imports `dart:ffi` at all.
///
/// Ownership rules from `rust/include/anycodec.h` are enforced by construction:
/// every caller-owned pointer is released in a `finally` (or freed by the
/// enclosing [AnyCodecArena]), and every borrowed pointer is copied out before
/// its owner is released.
library;

import 'dart:convert';
import 'dart:ffi';
import 'dart:typed_data';

import 'package:ffi/ffi.dart' as pkg_ffi;

import '../bindings/anycodec_bindings_generated.dart' as bindings;
import '../model/capability.dart';
import '../model/codec.dart';
import '../model/config.dart';
import '../model/frame.dart';
import '../model/format.dart';
import '../model/packet.dart';
import 'allocator.dart';
import 'status.dart';

/// Semantic version of the loaded native library.
final class NativeVersion {
  /// Builds a version triple plus its ABI revision.
  const NativeVersion({
    required this.abi,
    required this.major,
    required this.minor,
    required this.patch,
  });

  /// The C ABI revision the library was built against.
  final int abi;

  /// Major version.
  final int major;

  /// Minor version.
  final int minor;

  /// Patch version.
  final int patch;

  @override
  String toString() => '$major.$minor.$patch (abi $abi)';
}

/// A live native encoder handle.
///
/// The handle is thread-affine: it must be created, used and released on one
/// isolate. [NativeBindings.openEncoder] and friends are called from a worker
/// isolate, and [close] frees the handle there.
final class NativeEncoderHandle {
  NativeEncoderHandle._(this._pointer, this.codec, {required this.diagnostic});

  Pointer<bindings.AnyCodecEncoder>? _pointer;

  /// The codec this encoder was created for.
  final Codec codec;

  /// `true` when this is the diagnostic passthrough codec rather than a
  /// production one.
  final bool diagnostic;

  /// `true` once [close] has released the handle.
  bool get isClosed => _pointer == null;

  Pointer<bindings.AnyCodecEncoder> get _handle {
    final pointer = _pointer;
    if (pointer == null) {
      throw AnyCodecException.state(
        'this ${diagnostic ? 'diagnostic ' : ''}encoder has been closed',
        operation: 'encoder',
      );
    }
    return pointer;
  }

  /// How this encoder is doing its work.
  CodecPath get path => NativeBindings._encoderPath(_handle);

  /// The codec id the native handle reports, which must equal [codec].
  int get nativeCodec => bindings.anycodec_encoder_codec(_handle);

  /// Submits a picture; the plane bytes are copied into the session.
  void pushVideo(VideoFrame frame) => NativeBindings._pushVideo(_handle, frame);

  /// Submits a block of audio; the samples are copied into the session.
  void pushAudio(AudioFrame frame) => NativeBindings._pushAudio(_handle, frame);

  /// Drains one packet, or `null` when more input is required.
  Packet? receive() => NativeBindings._encoderReceive(_handle);

  /// Signals end of stream and drains everything that follows.
  List<Packet> flush() => NativeBindings._encoderFlush(_handle);

  /// Changes the target bitrate.
  void setBitrate(int bitsPerSecond) {
    final status = bindings.anycodec_encoder_set_bitrate(
      _handle,
      bitsPerSecond,
    );
    if (status != 0) {
      throw exceptionForStatus(status, 'anycodec_encoder_set_bitrate');
    }
  }

  /// Asks for a keyframe at the next opportunity.
  void requestKeyframe() {
    final status = bindings.anycodec_encoder_request_keyframe(_handle);
    if (status != 0) {
      throw exceptionForStatus(status, 'anycodec_encoder_request_keyframe');
    }
  }

  /// The codec configuration bytes, or `null` when there are none yet.
  Uint8List? extradata() => NativeBindings._encoderExtradata(_handle);

  /// Releases the handle. Idempotent.
  void close() {
    final pointer = _pointer;
    if (pointer != null) {
      _pointer = null;
      bindings.anycodec_encoder_free(pointer);
    }
  }

  @override
  String toString() =>
      'NativeEncoderHandle($codec, ${diagnostic ? 'diagnostic' : 'production'}, '
      '${isClosed ? 'closed' : 'open'})';
}

/// A live native decoder handle.
///
/// Same thread-affinity and ownership rules as [NativeEncoderHandle].
final class NativeDecoderHandle {
  NativeDecoderHandle._(this._pointer, this.codec, {required this.diagnostic});

  Pointer<bindings.AnyCodecDecoder>? _pointer;

  /// The codec this decoder was created for.
  final Codec codec;

  /// `true` when this is the diagnostic passthrough codec.
  final bool diagnostic;

  /// `true` once [close] has released the handle.
  bool get isClosed => _pointer == null;

  Pointer<bindings.AnyCodecDecoder> get _handle {
    final pointer = _pointer;
    if (pointer == null) {
      throw AnyCodecException.state(
        'this ${diagnostic ? 'diagnostic ' : ''}decoder has been closed',
        operation: 'decoder',
      );
    }
    return pointer;
  }

  /// How this decoder is doing its work.
  CodecPath get path => NativeBindings._decoderPath(_handle);

  /// The codec id the native handle reports, which must equal [codec].
  int get nativeCodec => bindings.anycodec_decoder_codec(_handle);

  /// Submits one compressed packet.
  void push(Packet packet) => NativeBindings._decoderPush(_handle, packet);

  /// Drains one frame, or `null` when more input is required.
  Frame? receive() => NativeBindings._decoderReceive(_handle);

  /// Signals end of stream and drains everything that follows.
  List<Frame> flush() => NativeBindings._decoderFlush(_handle);

  /// Releases the handle. Idempotent.
  void close() {
    final pointer = _pointer;
    if (pointer != null) {
      _pointer = null;
      bindings.anycodec_decoder_free(pointer);
    }
  }

  @override
  String toString() =>
      'NativeDecoderHandle($codec, ${diagnostic ? 'diagnostic' : 'production'}, '
      '${isClosed ? 'closed' : 'open'})';
}

/// Thin, typed wrappers over the generated C bindings.
///
/// This class is `abstract final`: it exists as a namespace, and it never holds
/// state, because native state belongs to whichever worker isolate made the
/// calls.
abstract final class NativeBindings {
  /// The ABI revision these bindings were generated for.
  static const int expectedAbiVersion = bindings.ANYCODEC_ABI_VERSION;

  /// The ABI revision the loaded library reports.
  static int abiVersion() => bindings.anycodec_abi_version();

  /// Refuses to continue when the library and the bindings disagree.
  ///
  /// A mismatch means the two sides disagree about struct layout, which is
  /// memory corruption waiting to happen; turning it into an exception is the
  /// whole point of the ABI revision.
  static void ensureCompatibleAbi() {
    final actual = abiVersion();
    if (actual != expectedAbiVersion) {
      throw AnyCodecException.internal(
        'the native library reports ABI version $actual but the Dart bindings '
        'were generated for $expectedAbiVersion; regenerate them with '
        '`dart run ffigen --config ffigen.yaml`',
        operation: 'anycodec_abi_version',
      );
    }
  }

  /// The library's version, read from the ABI.
  static NativeVersion version() => AnyCodecArena.scope((arena) {
    final out = arena.allocate<bindings.AnyCodecVersion>(
      sizeOf<bindings.AnyCodecVersion>(),
    );
    final status = bindings.anycodec_version(out);
    if (status != 0) {
      throw exceptionForStatus(status, 'anycodec_version');
    }
    final raw = out.ref;
    return NativeVersion(
      abi: raw.abi,
      major: raw.major,
      minor: raw.minor,
      patch: raw.patch,
    );
  });

  /// The library's version string. Never changes and is never freed.
  static String versionString() =>
      _borrowedString(bindings.anycodec_version_string());

  /// The codec slot the diagnostic codec advertises through the registry-facing
  /// API.
  ///
  /// The diagnostic codec is deliberately *not* registered against a production
  /// codec, so this is a hint for logging only.
  static int passthroughCodecId() => bindings.anycodec_passthrough_codec_id();

  /// The human readable capability report.
  ///
  /// Copied immediately: the native pointer lives in a thread-local slot that
  /// the next AnyCodec call overwrites.
  static String capabilityReport() =>
      _borrowedString(bindings.anycodec_capability_summary());

  /// The capability report for all twelve codecs, in both directions.
  static List<CodecCapability> capabilities() {
    final total = bindings.anycodec_capabilities(nullptr, 0);
    if (total == 0) {
      return const <CodecCapability>[];
    }
    return AnyCodecArena.scope((arena) {
      final out = arena.allocate<bindings.AnyCodecCapability>(
        sizeOf<bindings.AnyCodecCapability>() * total,
      );
      final written = bindings.anycodec_capabilities(out, total);
      final count = written < total ? written : total;
      final result = <CodecCapability>[];
      for (var index = 0; index < count; index++) {
        final raw = out[index];
        final codec = Codec.fromRaw(raw.codec);
        final direction = CodecDirection.fromRaw(raw.kind);
        if (codec == null || direction == null) {
          continue;
        }
        result.add(
          CodecCapability(
            codec: codec,
            direction: direction,
            backends: backendInfo(codec, direction),
          ),
        );
      }
      return result;
    });
  }

  /// The per-backend detail for one codec and direction.
  static List<BackendCapability> backendInfo(
    Codec codec,
    CodecDirection direction,
  ) {
    final total = bindings.anycodec_capability_backends(
      codec.raw,
      direction.raw,
      nullptr,
      0,
    );
    if (total == 0) {
      return const <BackendCapability>[];
    }
    return AnyCodecArena.scope((arena) {
      final out = arena.allocate<bindings.AnyCodecBackendInfo>(
        sizeOf<bindings.AnyCodecBackendInfo>() * total,
      );
      final written = bindings.anycodec_capability_backends(
        codec.raw,
        direction.raw,
        out,
        total,
      );
      final count = written < total ? written : total;
      final result = <BackendCapability>[];
      for (var index = 0; index < count; index++) {
        final raw = out[index];
        result.add(
          BackendCapability(
            backend: _backend(raw.backend),
            implementation: _implementation(raw.implementation),
            hardware: raw.hardware != 0,
            available: raw.available != 0,
            note: _fixedString(raw.note),
          ),
        );
      }
      return result;
    });
  }

  /// Probes the machine's hardware media engines.
  ///
  /// This is the one ABI call the header itself calls out as potentially slow
  /// (it can touch the GPU), so it is always issued from a worker isolate.
  static List<HardwareDevice> probeHardware() {
    final list = bindings.anycodec_probe_devices();
    if (list == nullptr) {
      return const <HardwareDevice>[];
    }
    try {
      return AnyCodecArena.scope((arena) {
        final count = bindings.anycodec_device_list_len(list);
        final info = arena.allocate<bindings.AnyCodecDeviceInfo>(
          sizeOf<bindings.AnyCodecDeviceInfo>(),
        );
        final codecSlot = arena.allocate<bindings.AnyCodecDeviceCodec>(
          sizeOf<bindings.AnyCodecDeviceCodec>(),
        );
        final devices = <HardwareDevice>[];
        for (var index = 0; index < count; index++) {
          final status = bindings.anycodec_device_list_device(
            list,
            index,
            info,
          );
          if (status != 0) {
            continue;
          }
          final raw = info.ref;
          final codecs = <HardwareDeviceCodec>[];
          for (var codecIndex = 0; codecIndex < raw.codec_count; codecIndex++) {
            final codecStatus = bindings.anycodec_device_list_codec(
              list,
              index,
              codecIndex,
              codecSlot,
            );
            if (codecStatus != 0) {
              continue;
            }
            final rawCodec = codecSlot.ref;
            final codec = Codec.fromRaw(rawCodec.codec);
            if (codec == null) {
              continue;
            }
            codecs.add(
              HardwareDeviceCodec(
                backend: _backend(rawCodec.backend),
                codec: codec,
                canDecode: rawCodec.decode != 0,
                canEncode: rawCodec.encode != 0,
                maxWidth: rawCodec.max_width,
                maxHeight: rawCodec.max_height,
                maxBitDepth: rawCodec.max_bit_depth,
              ),
            );
          }
          devices.add(
            HardwareDevice(
              backend: _backend(raw.backend),
              name: _fixedString(raw.name),
              apiVersion: _fixedString(raw.api_version),
              driverVersion: _fixedString(raw.driver_version),
              codecs: codecs,
            ),
          );
        }
        return devices;
      });
    } finally {
      bindings.anycodec_device_list_free(list);
    }
  }

  /// Creates a production encoder.
  ///
  /// Throws [AnyCodecException] when the native side refuses (an unknown codec,
  /// an unknown preference, or no usable backend); the message is the one the
  /// Rust registry built, which names every backend it tried.
  static NativeEncoderHandle openEncoder(
    EncoderConfig config,
    BackendPreference preference,
  ) => AnyCodecArena.scope((arena) {
    final pointer = arena.allocate<bindings.AnyCodecEncoderConfig>(
      sizeOf<bindings.AnyCodecEncoderConfig>(),
    );
    _fillEncoderConfig(arena, pointer, config);
    final handle = bindings.anycodec_encoder_new(pointer, preference.raw);
    if (handle == nullptr) {
      throw exceptionForStatus(
        lastNativeErrorStatus(),
        'anycodec_encoder_new(${config.codec.name})',
      );
    }
    return NativeEncoderHandle._(handle, config.codec, diagnostic: false);
  });

  /// Creates a production decoder.
  static NativeDecoderHandle openDecoder(
    DecoderConfig config,
    BackendPreference preference,
  ) => AnyCodecArena.scope((arena) {
    final pointer = arena.allocate<bindings.AnyCodecDecoderConfig>(
      sizeOf<bindings.AnyCodecDecoderConfig>(),
    );
    _fillDecoderConfig(arena, pointer, config);
    final handle = bindings.anycodec_decoder_new(pointer, preference.raw);
    if (handle == nullptr) {
      throw exceptionForStatus(
        lastNativeErrorStatus(),
        'anycodec_decoder_new(${config.codec.name})',
      );
    }
    return NativeDecoderHandle._(handle, config.codec, diagnostic: false);
  });

  /// Creates the diagnostic store encoder, which needs no platform support.
  ///
  /// It is not reachable through the registry by design, so it has its own entry
  /// points; an unknown codec is still [AnyCodecErrorKind.invalidArgument].
  static NativeEncoderHandle openDiagnosticEncoder(Codec codec) {
    final handle = bindings.anycodec_passthrough_encoder_new(codec.raw);
    if (handle == nullptr) {
      throw exceptionForStatus(
        lastNativeErrorStatus(),
        'anycodec_passthrough_encoder_new(${codec.name})',
      );
    }
    return NativeEncoderHandle._(handle, codec, diagnostic: true);
  }

  /// Creates the diagnostic retrieve decoder.
  static NativeDecoderHandle openDiagnosticDecoder(Codec codec) {
    final handle = bindings.anycodec_passthrough_decoder_new(codec.raw);
    if (handle == nullptr) {
      throw exceptionForStatus(
        lastNativeErrorStatus(),
        'anycodec_passthrough_decoder_new(${codec.name})',
      );
    }
    return NativeDecoderHandle._(handle, codec, diagnostic: true);
  }

  // -------------------------------------------------------------------------
  // Configuration marshalling
  // -------------------------------------------------------------------------

  static void _fillEncoderConfig(
    AnyCodecArena arena,
    Pointer<bindings.AnyCodecEncoderConfig> pointer,
    EncoderConfig config,
  ) {
    final keys = config.options.keys.toList(growable: false);
    final values = <String>[for (final key in keys) config.options[key]!];
    pointer.ref
      ..codec = config.codec.raw
      ..width = config.width
      ..height = config.height
      ..pixel_format = config.pixelFormat.raw
      ..framerate_num = config.framerateNumerator
      ..framerate_den = config.framerateDenominator
      ..keyframe_interval = config.keyframeInterval
      ..sample_rate = config.sampleRate
      ..channels = config.channels.raw
      ..sample_format = config.sampleFormat.raw
      ..frame_duration_ms = config.frameDurationMs
      ..bitrate = config.bitrate
      ..quality = config.quality
      ..preset = config.preset.raw
      ..option_count = keys.length
      ..option_keys = arena.allocateStrings(keys)
      ..option_values = arena.allocateStrings(values);
  }

  static void _fillDecoderConfig(
    AnyCodecArena arena,
    Pointer<bindings.AnyCodecDecoderConfig> pointer,
    DecoderConfig config,
  ) {
    final extradata = config.extradata;
    final Pointer<UnsignedChar> extradataPointer;
    if (extradata == null || extradata.isEmpty) {
      extradataPointer = nullptr;
    } else {
      extradataPointer = arena.allocateBytes(extradata.length);
      extradataPointer
          .cast<Uint8>()
          .asTypedList(extradata.length)
          .setAll(0, extradata);
    }
    pointer.ref
      ..codec = config.codec.raw
      ..width = config.width
      ..height = config.height
      ..pixel_format = config.pixelFormat?.raw ?? 0
      ..sample_rate = config.sampleRate
      ..channels = config.channels?.raw ?? 0
      ..sample_format = config.sampleFormat?.raw ?? 0
      ..annex_b = config.annexB ? 1 : 0
      ..extradata = extradataPointer
      ..extradata_len = extradataPointer == nullptr ? 0 : extradata!.length;
  }

  // -------------------------------------------------------------------------
  // Paths
  // -------------------------------------------------------------------------

  static CodecPath _encoderPath(Pointer<bindings.AnyCodecEncoder> handle) =>
      AnyCodecArena.scope((arena) {
        final out = arena.allocate<bindings.AnyCodecCodecPath>(
          sizeOf<bindings.AnyCodecCodecPath>(),
        );
        final status = bindings.anycodec_encoder_path(handle, out);
        if (status != 0) {
          throw exceptionForStatus(status, 'anycodec_encoder_path');
        }
        return _readPath(out.ref);
      });

  static CodecPath _decoderPath(Pointer<bindings.AnyCodecDecoder> handle) =>
      AnyCodecArena.scope((arena) {
        final out = arena.allocate<bindings.AnyCodecCodecPath>(
          sizeOf<bindings.AnyCodecCodecPath>(),
        );
        final status = bindings.anycodec_decoder_path(handle, out);
        if (status != 0) {
          throw exceptionForStatus(status, 'anycodec_decoder_path');
        }
        return _readPath(out.ref);
      });

  static CodecPath _readPath(bindings.AnyCodecCodecPath raw) {
    final reason = _fixedString(raw.fallback_reason);
    return CodecPath(
      backend: _backend(raw.backend),
      implementation: _implementation(raw.implementation),
      hardwareAccelerated: raw.hardware_accelerated != 0,
      detail: _fixedString(raw.detail),
      fallbackReason: reason.isEmpty ? null : reason,
    );
  }

  // -------------------------------------------------------------------------
  // Frames in
  // -------------------------------------------------------------------------

  static void _pushVideo(
    Pointer<bindings.AnyCodecEncoder> handle,
    VideoFrame frame,
  ) {
    AnyCodecArena.scope((arena) {
      final planeCount = frame.planes.length;
      final planes = arena.allocate<Pointer<UnsignedChar>>(
        sizeOf<Pointer<Void>>() * planeCount,
      );
      final strides = arena.allocate<UintPtr>(sizeOf<UintPtr>() * planeCount);
      for (var index = 0; index < planeCount; index++) {
        final plane = frame.planes[index];
        final stride = frame.strides[index];
        // The Rust side reads `max(stride, default_stride) * plane_height` bytes
        // from each pointer. Check that here, before native code can read past
        // the end of a buffer: a short plane must be an exception, not a crash.
        final required = frame.pixelFormat.minimumPlaneLength(
          frame.width,
          frame.height,
          index,
          stride,
        );
        if (required == null) {
          throw AnyCodecException.invalidArgument(
            '${frame.pixelFormat.name} has no plane $index',
            operation: 'encoder.pushVideo',
          );
        }
        if (plane.length < required) {
          throw AnyCodecException.invalidArgument(
            'plane $index is ${plane.length} bytes but '
            '${frame.pixelFormat.name} ${frame.width}x${frame.height} needs at '
            'least $required bytes for stride $stride',
            operation: 'encoder.pushVideo',
          );
        }
        if (plane.isEmpty) {
          throw AnyCodecException.invalidArgument(
            'plane $index is empty',
            operation: 'encoder.pushVideo',
          );
        }
        final buffer = arena.allocateBytes(plane.length);
        buffer.cast<Uint8>().asTypedList(plane.length).setAll(0, plane);
        planes[index] = buffer;
        strides[index] = stride;
      }
      final descriptor = arena.allocate<bindings.AnyCodecVideoFrame>(
        sizeOf<bindings.AnyCodecVideoFrame>(),
      );
      descriptor.ref
        ..pixel_format = frame.pixelFormat.raw
        ..width = frame.width
        ..height = frame.height
        ..plane_count = planeCount
        ..keyframe = frame.keyframe ? 1 : 0
        ..pts = frame.pts
        ..duration = frame.duration
        ..planes = planes
        ..strides = strides;
      final status = bindings.anycodec_encoder_push_video(handle, descriptor);
      if (status != 0) {
        throw exceptionForStatus(status, 'anycodec_encoder_push_video');
      }
    });
  }

  static void _pushAudio(
    Pointer<bindings.AnyCodecEncoder> handle,
    AudioFrame frame,
  ) {
    AnyCodecArena.scope((arena) {
      final required = frame.channels.byteLength(
        frame.frames,
        frame.sampleFormat,
      );
      if (frame.data.length < required) {
        throw AnyCodecException.invalidArgument(
          'audio payload is ${frame.data.length} bytes but ${frame.frames} frames '
          'of ${frame.channels.channels} channel(s) in ${frame.sampleFormat.name} '
          'need $required bytes',
          operation: 'encoder.pushAudio',
        );
      }
      final buffer = arena.allocateBytes(frame.data.length);
      if (frame.data.isNotEmpty) {
        buffer
            .cast<Uint8>()
            .asTypedList(frame.data.length)
            .setAll(0, frame.data);
      }
      final descriptor = arena.allocate<bindings.AnyCodecAudioFrame>(
        sizeOf<bindings.AnyCodecAudioFrame>(),
      );
      descriptor.ref
        ..sample_format = frame.sampleFormat.raw
        ..sample_rate = frame.sampleRate
        ..channels = frame.channels.raw
        ..frames = frame.frames
        ..pts = frame.pts
        ..data = buffer
        ..len = frame.data.length;
      final status = bindings.anycodec_encoder_push_audio(handle, descriptor);
      if (status != 0) {
        throw exceptionForStatus(status, 'anycodec_encoder_push_audio');
      }
    });
  }

  // -------------------------------------------------------------------------
  // Outputs
  // -------------------------------------------------------------------------

  static Packet? _encoderReceive(Pointer<bindings.AnyCodecEncoder> handle) =>
      AnyCodecArena.scope((arena) {
        final slot = arena.allocateSlot<bindings.AnyCodecPacket>();
        final status = bindings.anycodec_encoder_receive(handle, slot);
        if (status == bindings.STATUS_NEED_MORE_INPUT) {
          return null;
        }
        if (status != bindings.STATUS_GOT_OUTPUT) {
          throw exceptionForStatus(status, 'anycodec_encoder_receive');
        }
        final packet = slot.value;
        if (packet == nullptr) {
          return null;
        }
        try {
          return _readPacket(packet);
        } finally {
          bindings.anycodec_packet_free(packet);
        }
      });

  static List<Packet> _encoderFlush(Pointer<bindings.AnyCodecEncoder> handle) {
    final status = bindings.anycodec_encoder_flush(handle);
    if (status != 0) {
      throw exceptionForStatus(status, 'anycodec_encoder_flush');
    }
    final packets = <Packet>[];
    while (true) {
      final packet = _encoderReceive(handle);
      if (packet == null) {
        return packets;
      }
      packets.add(packet);
    }
  }

  static Uint8List? _encoderExtradata(
    Pointer<bindings.AnyCodecEncoder> handle,
  ) => AnyCodecArena.scope((arena) {
    final slot = arena.allocateSlot<bindings.AnyCodecBuffer>();
    final status = bindings.anycodec_encoder_extradata(handle, slot);
    if (status == bindings.STATUS_NEED_MORE_INPUT) {
      return null;
    }
    if (status != bindings.STATUS_GOT_OUTPUT) {
      throw exceptionForStatus(status, 'anycodec_encoder_extradata');
    }
    final buffer = slot.value;
    if (buffer == nullptr) {
      return null;
    }
    try {
      final length = bindings.anycodec_buffer_len(buffer);
      return copyNativeBytes(bindings.anycodec_buffer_data(buffer), length);
    } finally {
      bindings.anycodec_buffer_free(buffer);
    }
  });

  static void _decoderPush(
    Pointer<bindings.AnyCodecDecoder> handle,
    Packet packet,
  ) {
    AnyCodecArena.scope((arena) {
      final buffer = arena.allocateBytes(packet.data.length);
      if (packet.data.isNotEmpty) {
        buffer
            .cast<Uint8>()
            .asTypedList(packet.data.length)
            .setAll(0, packet.data);
      }
      final status = bindings.anycodec_decoder_push_bytes(
        handle,
        buffer,
        packet.data.length,
        packet.pts,
        packet.flags.bits,
      );
      if (status != 0) {
        throw exceptionForStatus(status, 'anycodec_decoder_push_bytes');
      }
    });
  }

  static Frame? _decoderReceive(Pointer<bindings.AnyCodecDecoder> handle) =>
      AnyCodecArena.scope((arena) {
        final slot = arena.allocateSlot<bindings.AnyCodecFrame>();
        final status = bindings.anycodec_decoder_receive(handle, slot);
        if (status == bindings.STATUS_NEED_MORE_INPUT) {
          return null;
        }
        if (status != bindings.STATUS_GOT_OUTPUT) {
          throw exceptionForStatus(status, 'anycodec_decoder_receive');
        }
        final frame = slot.value;
        if (frame == nullptr) {
          return null;
        }
        try {
          return _readFrame(arena, frame);
        } finally {
          bindings.anycodec_frame_free(frame);
        }
      });

  static List<Frame> _decoderFlush(Pointer<bindings.AnyCodecDecoder> handle) {
    final status = bindings.anycodec_decoder_flush(handle);
    if (status != 0) {
      throw exceptionForStatus(status, 'anycodec_decoder_flush');
    }
    final frames = <Frame>[];
    while (true) {
      final frame = _decoderReceive(handle);
      if (frame == null) {
        return frames;
      }
      frames.add(frame);
    }
  }

  static Frame _readFrame(
    AnyCodecArena arena,
    Pointer<bindings.AnyCodecFrame> handle,
  ) {
    if (bindings.anycodec_frame_is_video(handle) != 0) {
      final info = arena.allocate<bindings.AnyCodecVideoFrameInfo>(
        sizeOf<bindings.AnyCodecVideoFrameInfo>(),
      );
      final status = bindings.anycodec_frame_video_info(handle, info);
      if (status != 0) {
        throw exceptionForStatus(status, 'anycodec_frame_video_info');
      }
      final raw = info.ref;
      final format = PixelFormat.fromRaw(raw.pixel_format);
      if (format == null) {
        throw AnyCodecException.internal(
          'the native library produced pixel format ${raw.pixel_format}, which '
          'these bindings do not know',
          operation: 'anycodec_frame_video_info',
        );
      }
      final planes = <Uint8List>[];
      final strides = <int>[];
      for (var index = 0; index < raw.plane_count; index++) {
        final pointer = bindings.anycodec_frame_video_plane(handle, index);
        final length = bindings.anycodec_frame_video_plane_len(handle, index);
        planes.add(copyNativeBytes(pointer, length));
        strides.add(bindings.anycodec_frame_video_stride(handle, index));
      }
      return VideoFrame(
        pixelFormat: format,
        width: raw.width,
        height: raw.height,
        planes: planes,
        strides: strides,
        pts: raw.pts,
        duration: raw.duration,
      );
    }

    final info = arena.allocate<bindings.AnyCodecAudioFrameInfo>(
      sizeOf<bindings.AnyCodecAudioFrameInfo>(),
    );
    final status = bindings.anycodec_frame_audio_info(handle, info);
    if (status != 0) {
      throw exceptionForStatus(status, 'anycodec_frame_audio_info');
    }
    final raw = info.ref;
    final format = SampleFormat.fromRaw(raw.sample_format);
    final channels = ChannelLayout.fromRaw(raw.channels);
    if (format == null || channels == null) {
      throw AnyCodecException.internal(
        'the native library produced sample format ${raw.sample_format} / '
        'channel layout ${raw.channels}, which these bindings do not know',
        operation: 'anycodec_frame_audio_info',
      );
    }
    final pointer = bindings.anycodec_frame_audio_data(handle);
    final length = bindings.anycodec_frame_audio_len(handle);
    return AudioFrame(
      sampleFormat: format,
      sampleRate: raw.sample_rate,
      channels: channels,
      frames: raw.frames,
      data: copyNativeBytes(pointer, length),
      pts: raw.pts,
    );
  }

  static Packet _readPacket(Pointer<bindings.AnyCodecPacket> handle) {
    final codec = Codec.fromRaw(bindings.anycodec_packet_codec(handle));
    if (codec == null) {
      throw AnyCodecException.internal(
        'the native library produced a packet for an unknown codec',
        operation: 'anycodec_packet_codec',
      );
    }
    return Packet(
      codec: codec,
      data: copyNativeBytes(
        bindings.anycodec_packet_data(handle),
        bindings.anycodec_packet_len(handle),
      ),
      pts: bindings.anycodec_packet_pts(handle),
      dts: bindings.anycodec_packet_dts(handle),
      duration: bindings.anycodec_packet_duration(handle),
      flags: PacketFlags(bindings.anycodec_packet_flags(handle)),
    );
  }

  // -------------------------------------------------------------------------
  // Small helpers
  // -------------------------------------------------------------------------

  /// Copies a borrowed `const char*` into a Dart string.
  static String _borrowedString(Pointer<Char> pointer) =>
      pointer == nullptr ? '' : pointer.cast<pkg_ffi.Utf8>().toDartString();

  /// Decodes an inline `char[N]` field as UTF-8, stopping at the NUL.
  static String _fixedString(Array<Char> array) {
    final bytes = <int>[];
    for (final element in array.elements) {
      final value = element & 0xff;
      if (value == 0) {
        break;
      }
      bytes.add(value);
    }
    return utf8.decode(bytes, allowMalformed: true);
  }

  static Backend _backend(int raw) => Backend.fromRaw(raw) ?? Backend.external;

  static Implementation _implementation(int raw) =>
      Implementation.fromRaw(raw) ?? Implementation.native;
}
