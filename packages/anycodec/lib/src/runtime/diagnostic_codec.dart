/// A pure-Dart copy of the diagnostic codec, for platforms with no Rust core.
///
/// The native runtime uses the real Rust `passthrough` codec
/// (`rust/src/soft/builtin/passthrough.rs`) through its dedicated C entry points.
/// The web runtime cannot: there is no `dart:ffi` in a browser. This file
/// implements the *same* wire format in Dart, so the diagnostic codec means the
/// same thing -and produces the same bytes -everywhere:
///
/// ```text
/// packet := is_video:u8 | body_length:u32le | body
/// video  := width:u32le | height:u32le | pixel_format:u32le | plane_count:u32le
///           | (stride:u32le | length:u32le) * plane_count
///           | plane bytes, concatenated in plane order
/// audio  := sample_rate:u32le | channels:u32le | sample_format:u32le
///           | frames:u32le | interleaved samples
/// ```
///
/// Like the Rust codec it neither compresses nor decompresses: it stores each
/// frame verbatim. Its value is that it exercises timestamps, buffer ownership,
/// flush semantics and the whole session API on a machine (or a browser) with no
/// media framework at all.
library;

import 'dart:typed_data';

import '../model/capability.dart';
import '../model/codec.dart';
import '../model/frame.dart';
import '../model/format.dart';
import '../model/packet.dart';
import '../model/error.dart';
import 'codec_runtime.dart';

/// The `CodecPath` every diagnostic session reports.
///
/// It is deliberately the same shape as the Rust codec's: backend `rust`,
/// software implementation, and a `detail` that names the codec so a caller can
/// tell at a glance that this is the diagnostic path rather than a real
/// `libavcodec` or Media Foundation session.
const CodecPath diagnosticCodecPath = CodecPath(
  backend: Backend.rust,
  implementation: Implementation.software,
  detail: 'passthrough (verbatim store/retrieve)',
);

/// Stores frames verbatim, exactly like the Rust diagnostic encoder.
final class DiagnosticEncoder implements AnycodecEncoder {
  /// Builds an encoder for [codec].
  DiagnosticEncoder(this.codec);

  @override
  final Codec codec;

  @override
  CodecPath get path => diagnosticCodecPath;

  @override
  int get workerId => 0;

  bool _closed = false;
  int _outstanding = 0;
  final List<Packet> _pending = <Packet>[];

  @override
  bool get isClosed => _closed;

  @override
  Future<void> encode(Frame frame) async {
    _requireOpen('encode');
    final framed = _frameToPayload(frame);
    final body = framed.body;
    final packet = BytesBuilder(copy: false)
      ..addByte(framed.isVideo ? 1 : 0)
      ..add(_u32(body.length))
      ..add(body);
    _pending.add(
      Packet(
        codec: codec,
        data: packet.takeBytes(),
        pts: frame.pts,
        dts: frame.pts,
        duration: frame.duration,
        flags: PacketFlags.keyframe,
      ),
    );
    _outstanding++;
  }

  @override
  Future<Packet?> receive() async {
    _requireOpen('receive');
    if (_pending.isEmpty) {
      return null;
    }
    if (_outstanding > 0) {
      _outstanding--;
    }
    return _pending.removeAt(0);
  }

  @override
  Future<List<Packet>> flush() async {
    _requireOpen('flush');
    final packets = List<Packet>.of(_pending);
    _pending.clear();
    _outstanding = 0;
    return packets;
  }

  @override
  Future<int> bufferedPackets() async => _outstanding;

  @override
  Future<void> setBitrate(int bitsPerSecond) async =>
      throw AnyCodecException.unsupported(
        'the diagnostic codec does not compress, so it has no bitrate to set',
        operation: 'setBitrate',
      );

  @override
  Future<void> requestKeyframe() async => throw AnyCodecException.unsupported(
    'the diagnostic codec emits every frame as a keyframe',
    operation: 'requestKeyframe',
  );

  @override
  Future<Uint8List?> extradata() async {
    _requireOpen('extradata');
    return null;
  }

  @override
  Future<void> close() async {
    _closed = true;
    _pending.clear();
    _outstanding = 0;
  }

  void _requireOpen(String operation) {
    if (_closed) {
      throw AnyCodecException.state(
        'this session has been closed',
        operation: operation,
      );
    }
  }
}

/// Rebuilds frames stored by [DiagnosticEncoder] (or by the Rust codec).
final class DiagnosticDecoder implements AnycodecDecoder {
  /// Builds a decoder for [codec].
  DiagnosticDecoder(this.codec);

  @override
  final Codec codec;

  @override
  CodecPath get path => diagnosticCodecPath;

  @override
  int get workerId => 0;

  bool _closed = false;
  final List<Frame> _pending = <Frame>[];

  @override
  bool get isClosed => _closed;

  @override
  Future<void> decode(Packet packet) async {
    _requireOpen('decode');
    final bytes = packet.data;
    if (bytes.length < 5) {
      throw AnyCodecException(
        kind: AnyCodecErrorKind.backend,
        message: 'passthrough packet is shorter than its header',
        status: AnyCodecErrorKind.backend.status,
        operation: 'decode',
      );
    }
    final isVideo = bytes[0] != 0;
    final length = _readU32(bytes, 1);
    if (bytes.length < 5 + length) {
      throw AnyCodecException(
        kind: AnyCodecErrorKind.backend,
        message:
            'passthrough packet declares $length bytes but carries '
            '${bytes.length - 5}',
        status: AnyCodecErrorKind.backend.status,
        operation: 'decode',
      );
    }
    final body = Uint8List.sublistView(bytes, 5, 5 + length);
    _pending.add(
      isVideo ? _decodeVideo(body, packet.pts) : _decodeAudio(body, packet.pts),
    );
  }

  @override
  Future<Frame?> receive() async {
    _requireOpen('receive');
    return _pending.isEmpty ? null : _pending.removeAt(0);
  }

  @override
  Future<List<Frame>> flush() async {
    _requireOpen('flush');
    final frames = List<Frame>.of(_pending);
    _pending.clear();
    return frames;
  }

  @override
  Future<void> close() async {
    _closed = true;
    _pending.clear();
  }

  void _requireOpen(String operation) {
    if (_closed) {
      throw AnyCodecException.state(
        'this session has been closed',
        operation: operation,
      );
    }
  }
}

/// One encoded diagnostic packet body.
final class _DiagnosticBody {
  const _DiagnosticBody(this.isVideo, this.body);

  final bool isVideo;
  final Uint8List body;
}

_DiagnosticBody _frameToPayload(Frame frame) {
  switch (frame) {
    case VideoFrame():
      final planes = <Uint8List>[];
      // The native path coerces each stride up to the packed stride and keeps
      // exactly `stride * rows` bytes, so mirror that here: the packet is then
      // byte-identical to the one the Rust codec would have produced.
      final geometry = <(int, int)>[];
      for (var index = 0; index < frame.planes.length; index++) {
        final stride = frame.pixelFormat.effectiveStride(
          frame.width,
          index,
          frame.strides[index],
        );
        if (stride == null) {
          throw AnyCodecException.invalidArgument(
            '${frame.pixelFormat.name} has no plane $index',
            operation: 'encode',
          );
        }
        final rows = frame.pixelFormat.planeHeight(frame.height, index)!;
        final length = stride * rows;
        final plane = frame.planes[index];
        if (plane.length < length) {
          throw AnyCodecException.invalidArgument(
            'plane $index is ${plane.length} bytes but ${frame.width}x'
            '${frame.height} ${frame.pixelFormat.name} needs $length for '
            'stride $stride',
            operation: 'encode',
          );
        }
        geometry.add((stride, length));
        planes.add(
          length == plane.length
              ? Uint8List.fromList(plane)
              : Uint8List.sublistView(plane, 0, length),
        );
      }
      final body = BytesBuilder()
        ..add(_u32(frame.width))
        ..add(_u32(frame.height))
        ..add(_u32(frame.pixelFormat.raw))
        ..add(_u32(frame.planes.length));
      for (final (stride, length) in geometry) {
        body
          ..add(_u32(stride))
          ..add(_u32(length));
      }
      for (final plane in planes) {
        body.add(plane);
      }
      return _DiagnosticBody(true, body.takeBytes());

    case AudioFrame():
      final expected = frame.channels.byteLength(
        frame.frames,
        frame.sampleFormat,
      );
      if (frame.data.length < expected) {
        throw AnyCodecException.invalidArgument(
          'audio payload is ${frame.data.length} bytes but ${frame.frames} '
          'frames of ${frame.channels.channels} channel(s) need $expected bytes',
          operation: 'encode',
        );
      }
      final body = BytesBuilder()
        ..add(_u32(frame.sampleRate))
        ..add(_u32(frame.channels.raw))
        ..add(_u32(frame.sampleFormat.raw))
        ..add(_u32(frame.frames))
        ..add(
          expected == frame.data.length
              ? Uint8List.fromList(frame.data)
              : Uint8List.sublistView(frame.data, 0, expected),
        );
      return _DiagnosticBody(false, body.takeBytes());
  }
}

VideoFrame _decodeVideo(Uint8List body, int pts) {
  final reader = _Reader(body);
  final width = reader.u32();
  final height = reader.u32();
  final format = PixelFormat.fromRaw(reader.u32());
  final planeCount = reader.u32();
  if (format == null) {
    throw AnyCodecException(
      kind: AnyCodecErrorKind.backend,
      message: 'unknown pixel format in a passthrough packet',
      status: AnyCodecErrorKind.backend.status,
      operation: 'decode',
    );
  }
  if (planeCount != format.planeCount) {
    throw AnyCodecException(
      kind: AnyCodecErrorKind.backend,
      message:
          '${format.name} needs ${format.planeCount} planes but the packet '
          'declares $planeCount',
      status: AnyCodecErrorKind.backend.status,
      operation: 'decode',
    );
  }
  final geometry = <(int, int)>[];
  for (var index = 0; index < planeCount; index++) {
    geometry.add((reader.u32(), reader.u32()));
  }
  final planes = <Uint8List>[];
  final strides = <int>[];
  for (final (stride, length) in geometry) {
    planes.add(reader.bytes(length));
    strides.add(stride);
  }
  return VideoFrame(
    pixelFormat: format,
    width: width,
    height: height,
    planes: planes,
    strides: strides,
    pts: pts,
  );
}

AudioFrame _decodeAudio(Uint8List body, int pts) {
  final reader = _Reader(body);
  final sampleRate = reader.u32();
  final channels = ChannelLayout.fromRaw(reader.u32());
  final format = SampleFormat.fromRaw(reader.u32());
  final frames = reader.u32();
  if (channels == null || format == null) {
    throw AnyCodecException(
      kind: AnyCodecErrorKind.backend,
      message:
          'unknown sample format or channel layout in a passthrough packet',
      status: AnyCodecErrorKind.backend.status,
      operation: 'decode',
    );
  }
  return AudioFrame(
    sampleFormat: format,
    sampleRate: sampleRate,
    channels: channels,
    frames: frames,
    data: reader.rest(),
    pts: pts,
  );
}

Uint8List _u32(int value) {
  final bytes = Uint8List(4);
  ByteData.view(bytes.buffer).setUint32(0, value, Endian.little);
  return bytes;
}

int _readU32(Uint8List bytes, int offset) => ByteData.view(
  bytes.buffer,
  bytes.offsetInBytes + offset,
  4,
).getUint32(0, Endian.little);

/// A bounds-checked little-endian reader over a packet body.
final class _Reader {
  _Reader(this._bytes);

  final Uint8List _bytes;
  int _cursor = 0;

  int u32() {
    if (_cursor + 4 > _bytes.length) {
      throw AnyCodecException(
        kind: AnyCodecErrorKind.backend,
        message: 'truncated passthrough header',
        status: AnyCodecErrorKind.backend.status,
        operation: 'decode',
      );
    }
    final value = _readU32(_bytes, _cursor);
    _cursor += 4;
    return value;
  }

  Uint8List bytes(int length) {
    if (_cursor + length > _bytes.length) {
      throw AnyCodecException(
        kind: AnyCodecErrorKind.backend,
        message: 'truncated passthrough plane',
        status: AnyCodecErrorKind.backend.status,
        operation: 'decode',
      );
    }
    final slice = Uint8List.fromList(
      Uint8List.sublistView(_bytes, _cursor, _cursor + length),
    );
    _cursor += length;
    return slice;
  }

  Uint8List rest() => bytes(_bytes.length - _cursor);
}
