/// The web runtime: WebCodecs plus a pure-Dart diagnostic codec.
///
/// Two things are different on the web, and both are deliberate:
///
/// * there are no isolates to hide behind, so this runtime does the work
///   directly -every WebCodecs call is already asynchronous and never blocks
///   the event loop;
/// * `dart:ffi` does not exist, so the Rust core cannot be reached at all. The
///   capability report therefore describes what the *browser* can do, and the
///   diagnostic codec is [DiagnosticEncoder]/[DiagnosticDecoder] -a pure-Dart
///   implementation of the same wire format the Rust codec uses.
///
/// `package:web` 1.1.1 exposes WebCodecs **video** only (`VideoEncoder`,
/// `VideoDecoder`, `VideoFrame`, `EncodedVideoChunk`). There is no
/// `AudioEncoder`/`AudioDecoder` binding, so audio sessions throw
/// `AnyCodecException` with `kind == unsupported` and the capability report says
/// so instead of pretending otherwise.
library;

import 'dart:async';
import 'dart:js_interop';
import 'dart:js_interop_unsafe';
import 'dart:typed_data';

import 'package:web/web.dart' as web;

import '../model/capability.dart';
import '../model/codec.dart';
import '../model/config.dart';
import '../model/error.dart';
import '../model/frame.dart';
import '../model/format.dart';
import '../model/packet.dart';
import '../platform/platform_web.dart';
import 'codec_runtime.dart';
import 'diagnostic_codec.dart';

/// Creates the browser runtime.
CodecRuntime createRuntime() => WebCodecRuntime();

/// The `dart:js_interop` runtime.
final class WebCodecRuntime implements CodecRuntime {
  final List<Future<void> Function()> _closers = <Future<void> Function()>[];

  @override
  int get workerCount => 0;

  @override
  int get passthroughCodecId => Codec.h266.raw;

  @override
  Future<void> initialize({int workers = 0}) async {
    // Nothing to do: the browser already has as many threads as it will give us,
    // and `dart:isolate` is not available here.
  }

  @override
  Future<void> shutdown() async {
    final closers = List<Future<void> Function()>.of(_closers);
    _closers.clear();
    for (final close in closers) {
      await close();
    }
  }

  @override
  Future<List<CodecCapability>> capabilities() async {
    final capabilities = <CodecCapability>[];
    for (final codec in Codec.values) {
      capabilities.add(
        CodecCapability(
          codec: codec,
          direction: CodecDirection.encode,
          backends: <BackendCapability>[
            await _probe(codec, CodecDirection.encode),
          ],
        ),
      );
      capabilities.add(
        CodecCapability(
          codec: codec,
          direction: CodecDirection.decode,
          backends: <BackendCapability>[
            await _probe(codec, CodecDirection.decode),
          ],
        ),
      );
    }
    return capabilities;
  }

  @override
  Future<List<HardwareDevice>> probeHardware() async {
    // WebCodecs deliberately hides the machine's media engines: there is no API
    // to enumerate them, and a browser will not tell a page which GPU a codec
    // ran on. An empty list is the honest answer.
    return const <HardwareDevice>[];
  }

  @override
  Future<String> capabilityReport() async {
    final capabilities = await this.capabilities();
    final buffer = StringBuffer()
      ..writeln('AnyCodec web runtime (WebCodecs)')
      ..writeln('--------------------------------');
    final available = capabilities.where((entry) => entry.isAvailable).length;
    buffer.writeln(
      '$available of ${capabilities.length} codec/direction pairs usable',
    );
    if (available == 0) {
      buffer.writeln(
        '(this browser exposes no WebCodecs support for any codec in this '
        'report; the diagnostic codec still works)',
      );
    }
    for (final entry in capabilities) {
      buffer.writeln(entry.summary);
    }
    return buffer.toString();
  }

  @override
  Future<String> platformReport() async {
    final buffer = StringBuffer()
      ..writeln('web runtime (dart:js_interop / WebCodecs)')
      ..writeln('---------------------------------------')
      ..writeln('isolates            : none available in a browser')
      ..writeln('VideoEncoder/Decoder: ${WebCodecs.videoSupported}')
      ..writeln('AudioEncoder/Decoder: ${WebCodecs.audioSupported}')
      ..writeln('diagnostic codec id : $passthroughCodecId (internal slot)')
      ..writeln('worker isolates     : $workerCount');
    final support = await WebCodecs.capabilities();
    for (final entry in support) {
      buffer.writeln(
        '${entry.codec.name.padRight(7)}: '
        '${entry.supported ? 'encode=${entry.videoEncode} decode=${entry.videoDecode}' : entry.note}',
      );
    }
    return buffer.toString();
  }

  @override
  Future<AnycodecEncoder> encoder(
    EncoderConfig config, {
    BackendPreference preference = BackendPreference.auto,
  }) async {
    if (!config.codec.isVideo) {
      throw AnyCodecException.unsupported(
        'the web runtime cannot encode ${config.codec.name}: package:web '
        'exposes WebCodecs video only (no AudioEncoder binding)',
        operation: 'encoder',
      );
    }
    if (preference == BackendPreference.softwareOnly) {
      throw AnyCodecException.unsupported(
        'WebCodecs offers no software-only encoder selection; use '
        'BackendPreference.auto',
        operation: 'encoder',
      );
    }
    final session = await _WebVideoEncoder.open(config, preference);
    _closers.add(session.close);
    return session;
  }

  @override
  Future<AnycodecDecoder> decoder(
    DecoderConfig config, {
    BackendPreference preference = BackendPreference.auto,
  }) async {
    if (!config.codec.isVideo) {
      throw AnyCodecException.unsupported(
        'the web runtime cannot decode ${config.codec.name}: package:web '
        'exposes WebCodecs video only (no AudioDecoder binding)',
        operation: 'decoder',
      );
    }
    final session = await _WebVideoDecoder.open(config, preference);
    _closers.add(session.close);
    return session;
  }

  @override
  Future<AnycodecEncoder> diagnosticEncoder(Codec codec) async {
    final session = DiagnosticEncoder(codec);
    _closers.add(session.close);
    return session;
  }

  @override
  Future<AnycodecDecoder> diagnosticDecoder(Codec codec) async {
    final session = DiagnosticDecoder(codec);
    _closers.add(session.close);
    return session;
  }

  Future<BackendCapability> _probe(
    Codec codec,
    CodecDirection direction,
  ) async {
    const backend = Backend.webCodecs;
    const implementation = Implementation.native;
    if (!codec.isVideo) {
      return BackendCapability(
        backend: backend,
        implementation: implementation,
        hardware: false,
        available: false,
        note:
            'the WebCodecs audio bindings are not available in package:web '
            '1.1.1',
      );
    }
    final codecString = webCodecStringFor(codec);
    if (codecString == null) {
      return BackendCapability(
        backend: backend,
        implementation: implementation,
        hardware: false,
        available: false,
        note: 'WebCodecs has no codec string for ${codec.name}',
      );
    }
    try {
      final support = direction == CodecDirection.encode
          ? await WebCodecs.supportsVideoEncoder(
              codec: codec,
              width: 640,
              height: 480,
            )
          : await WebCodecs.supportsVideoDecoder(codec: codec);
      return BackendCapability(
        backend: backend,
        implementation: implementation,
        hardware: false,
        available: support.supported,
        note: support.supported
            ? 'ready (${support.note}; whether the GPU is used is not reported '
                  'by WebCodecs)'
            : support.note,
      );
    } catch (error) {
      return BackendCapability(
        backend: backend,
        implementation: implementation,
        hardware: false,
        available: false,
        note: 'WebCodecs probe failed: $error',
      );
    }
  }
}

/// One encoded chunk, copied out of the callback so it outlives it.
final class _WebChunk {
  const _WebChunk(this.data, this.keyframe, this.timestamp, this.duration);

  final Uint8List data;
  final bool keyframe;
  final int timestamp;
  final int duration;
}

/// A WebCodecs `VideoEncoder` session.
final class _WebVideoEncoder implements AnycodecEncoder {
  _WebVideoEncoder._(this.codec, this._encoder, this.path);

  static Future<_WebVideoEncoder> open(
    EncoderConfig config,
    BackendPreference preference,
  ) async {
    final codecString = webCodecStringFor(config.codec);
    if (codecString == null) {
      throw AnyCodecException.unsupported(
        'WebCodecs has no codec string for ${config.codec.name}',
        operation: 'encoder',
      );
    }
    final session = _WebVideoEncoder._(
      config.codec,
      null,
      const CodecPath(
        backend: Backend.webCodecs,
        implementation: Implementation.native,
        detail: 'web: VideoEncoder',
      ),
    );
    final hardware = webHardwareHint(preference);
    final webConfig = web.VideoEncoderConfig(
      codec: codecString,
      width: config.width,
      height: config.height,
      bitrate: config.bitrate == 0 ? 1000000 : config.bitrate,
      framerate: config.framerateDenominator == 0
          ? config.framerateNumerator.toDouble()
          : config.framerateNumerator / config.framerateDenominator,
      hardwareAcceleration: hardware,
    );
    final support = await web.VideoEncoder.isConfigSupported(webConfig).toDart;
    if (!support.supported) {
      throw AnyCodecException.unavailable(
        'this browser cannot configure a WebCodecs encoder for $codecString '
        '${config.width}x${config.height}',
        operation: 'encoder',
      );
    }
    final encoder = web.VideoEncoder(
      web.VideoEncoderInit(
        output: ((web.EncodedVideoChunk chunk, JSAny? metadata) {
          session._onChunk(chunk, metadata);
        }).toJS,
        error: ((JSAny error) {
          session._failure = AnyCodecException.internal(
            'WebCodecs encoder error: ${error.dartify()}',
            operation: 'encoder',
          );
        }).toJS,
      ),
    );
    encoder.configure(webConfig);
    session._encoder = encoder;
    session.path = CodecPath(
      backend: Backend.webCodecs,
      implementation: Implementation.native,
      hardwareAccelerated: preference == BackendPreference.requireHardware,
      detail: 'web: VideoEncoder $codecString ${config.width}x${config.height}',
    );
    return session;
  }

  @override
  final Codec codec;

  web.VideoEncoder? _encoder;

  @override
  CodecPath path;

  final List<_WebChunk> _outputs = <_WebChunk>[];
  Uint8List? _extradata;
  AnyCodecException? _failure;
  bool _closed = false;
  int _outstanding = 0;

  @override
  int get workerId => 0;

  @override
  bool get isClosed => _closed;

  @override
  Future<void> encode(Frame frame) async {
    final encoder = _requireOpen('encode');
    if (frame is! VideoFrame) {
      throw AnyCodecException.invalidArgument(
        'the web runtime encodes pictures only, and was handed an AudioFrame',
        operation: 'encode',
      );
    }
    final format = webPixelFormatFor(frame.pixelFormat);
    if (format == null) {
      throw AnyCodecException.unsupported(
        'WebCodecs has no ${frame.pixelFormat.name} input format',
        operation: 'encode',
      );
    }
    final tight = _tightenPlanes(frame);
    final init = web.VideoFrameBufferInit(
      format: format,
      codedWidth: frame.width,
      codedHeight: frame.height,
      timestamp: frame.pts,
    );
    if (frame.duration != 0) {
      init.duration = frame.duration;
    }
    final videoFrame = web.VideoFrame(tight.toJS, init);
    try {
      if (frame.keyframe) {
        encoder.encode(
          videoFrame,
          web.VideoEncoderEncodeOptions(keyFrame: true),
        );
      } else {
        encoder.encode(videoFrame);
      }
      _outstanding++;
    } finally {
      videoFrame.close();
    }
  }

  @override
  Future<Packet?> receive() async {
    _requireOpen('receive');
    if (_outputs.isNotEmpty) {
      final chunk = _outputs.removeAt(0);
      if (_outstanding > 0) {
        _outstanding--;
      }
      return Packet(
        codec: codec,
        data: chunk.data,
        pts: chunk.timestamp,
        dts: chunk.timestamp,
        duration: chunk.duration,
        flags: chunk.keyframe ? PacketFlags.keyframe : PacketFlags.none,
      );
    }
    return null;
  }

  @override
  Future<List<Packet>> flush() async {
    final encoder = _requireOpen('flush');
    await encoder.flush().toDart;
    _outstanding = 0;
    final packets = <Packet>[
      for (final chunk in _outputs)
        Packet(
          codec: codec,
          data: chunk.data,
          pts: chunk.timestamp,
          dts: chunk.timestamp,
          duration: chunk.duration,
          flags: chunk.keyframe ? PacketFlags.keyframe : PacketFlags.none,
        ),
    ];
    _outputs.clear();
    return packets;
  }

  @override
  Future<int> bufferedPackets() async => _outstanding;

  @override
  Future<void> setBitrate(int bitsPerSecond) async {
    throw AnyCodecException.unsupported(
      'WebCodecs cannot retune a live VideoEncoder; reconfigure the session '
      'instead',
      operation: 'setBitrate',
    );
  }

  @override
  Future<void> requestKeyframe() async {
    _requireOpen('requestKeyframe');
    // WebCodecs expresses a keyframe request on the frame, not on the encoder.
    throw AnyCodecException.unsupported(
      'WebCodecs requests keyframes per frame: set VideoFrame.keyframe instead',
      operation: 'requestKeyframe',
    );
  }

  @override
  Future<Uint8List?> extradata() async {
    _requireOpen('extradata');
    return _extradata;
  }

  @override
  Future<void> close() async {
    if (_closed) {
      return;
    }
    _closed = true;
    final encoder = _encoder;
    _encoder = null;
    if (encoder != null && encoder.state != 'closed') {
      encoder.close();
    }
    _outputs.clear();
  }

  web.VideoEncoder _requireOpen(String operation) {
    if (_closed || _encoder == null) {
      throw AnyCodecException.state(
        'this session has been closed',
        operation: operation,
      );
    }
    final failure = _failure;
    if (failure != null) {
      throw failure;
    }
    return _encoder!;
  }

  void _onChunk(web.EncodedVideoChunk chunk, JSAny? metadata) {
    final bytes = Uint8List(chunk.byteLength);
    chunk.copyTo(bytes.toJS);
    _outputs.add(
      _WebChunk(
        bytes,
        chunk.type == 'key',
        chunk.timestamp,
        chunk.duration ?? 0,
      ),
    );
    _extradata ??= _descriptionFromMetadata(metadata);
  }
}

/// A WebCodecs `VideoDecoder` session.
final class _WebVideoDecoder implements AnycodecDecoder {
  _WebVideoDecoder._(this.codec, this._decoder, this.path);

  static Future<_WebVideoDecoder> open(
    DecoderConfig config,
    BackendPreference preference,
  ) async {
    final codecString = webCodecStringFor(config.codec);
    if (codecString == null) {
      throw AnyCodecException.unsupported(
        'WebCodecs has no codec string for ${config.codec.name}',
        operation: 'decoder',
      );
    }
    final session = _WebVideoDecoder._(
      config.codec,
      null,
      const CodecPath(
        backend: Backend.webCodecs,
        implementation: Implementation.native,
        detail: 'web: VideoDecoder',
      ),
    );
    final webConfig = web.VideoDecoderConfig(
      codec: codecString,
      hardwareAcceleration: webHardwareHint(preference),
    );
    final extradata = config.extradata;
    if (extradata != null && extradata.isNotEmpty) {
      webConfig.description = extradata.toJS;
    }
    if (config.width != 0) {
      webConfig.codedWidth = config.width;
    }
    if (config.height != 0) {
      webConfig.codedHeight = config.height;
    }
    final support = await web.VideoDecoder.isConfigSupported(webConfig).toDart;
    if (!support.supported) {
      throw AnyCodecException.unavailable(
        'this browser cannot configure a WebCodecs decoder for $codecString',
        operation: 'decoder',
      );
    }
    final decoder = web.VideoDecoder(
      web.VideoDecoderInit(
        output: ((web.VideoFrame frame, JSAny? metadata) {
          session._frames.add(frame);
        }).toJS,
        error: ((JSAny error) {
          session._failure = AnyCodecException.internal(
            'WebCodecs decoder error: ${error.dartify()}',
            operation: 'decoder',
          );
        }).toJS,
      ),
    );
    decoder.configure(webConfig);
    session._decoder = decoder;
    session.path = CodecPath(
      backend: Backend.webCodecs,
      implementation: Implementation.native,
      hardwareAccelerated: preference == BackendPreference.requireHardware,
      detail: 'web: VideoDecoder $codecString',
    );
    return session;
  }

  @override
  final Codec codec;

  web.VideoDecoder? _decoder;

  @override
  CodecPath path;

  final List<web.VideoFrame> _frames = <web.VideoFrame>[];
  AnyCodecException? _failure;
  bool _closed = false;

  @override
  int get workerId => 0;

  @override
  bool get isClosed => _closed;

  @override
  Future<void> decode(Packet packet) async {
    final decoder = _requireOpen('decode');
    final init = web.EncodedVideoChunkInit(
      type: packet.flags.isKeyframe ? 'key' : 'delta',
      timestamp: packet.pts,
      data: packet.data.toJS,
    );
    if (packet.duration != 0) {
      init.duration = packet.duration;
    }
    decoder.decode(web.EncodedVideoChunk(init));
  }

  @override
  Future<Frame?> receive() async {
    _requireOpen('receive');
    if (_frames.isEmpty) {
      return null;
    }
    final frame = _frames.removeAt(0);
    try {
      return await _convert(frame);
    } finally {
      frame.close();
    }
  }

  @override
  Future<List<Frame>> flush() async {
    final decoder = _requireOpen('flush');
    await decoder.flush().toDart;
    final frames = <Frame>[];
    while (_frames.isNotEmpty) {
      final frame = _frames.removeAt(0);
      try {
        frames.add(await _convert(frame));
      } finally {
        frame.close();
      }
    }
    return frames;
  }

  @override
  Future<void> close() async {
    if (_closed) {
      return;
    }
    _closed = true;
    final decoder = _decoder;
    _decoder = null;
    if (decoder != null && decoder.state != 'closed') {
      decoder.close();
    }
    for (final frame in _frames) {
      frame.close();
    }
    _frames.clear();
  }

  web.VideoDecoder _requireOpen(String operation) {
    if (_closed || _decoder == null) {
      throw AnyCodecException.state(
        'this session has been closed',
        operation: operation,
      );
    }
    final failure = _failure;
    if (failure != null) {
      throw failure;
    }
    return _decoder!;
  }

  Future<Frame> _convert(web.VideoFrame frame) async {
    final format = switch (frame.format) {
      'I420' => PixelFormat.i420,
      'NV12' => PixelFormat.nv12,
      'RGBA' => PixelFormat.rgba8,
      'BGRA' => PixelFormat.bgra8,
      _ => null,
    };
    if (format == null) {
      throw AnyCodecException.unsupported(
        'WebCodecs produced a ${frame.format ?? 'null'} frame, which the Dart '
        'layer has no pixel format for',
        operation: 'decode',
      );
    }
    final size = frame.allocationSize();
    final buffer = Uint8List(size);
    final layouts = await frame.copyTo(buffer.toJS).toDart;
    final heights = <int>[
      for (var index = 0; index < format.planeCount; index++)
        format.planeHeight(frame.codedHeight, index) ?? 0,
    ];
    final planes = <Uint8List>[];
    final strides = <int>[];
    for (var index = 0; index < format.planeCount; index++) {
      final layout = layouts[index];
      final rows = heights[index];
      final length = layout.stride * rows;
      planes.add(
        Uint8List.sublistView(buffer, layout.offset, layout.offset + length),
      );
      strides.add(layout.stride);
    }
    return VideoFrame(
      pixelFormat: format,
      width: frame.codedWidth,
      height: frame.codedHeight,
      planes: planes,
      strides: strides,
      pts: frame.timestamp,
      duration: frame.duration ?? 0,
    );
  }
}

/// Copies [frame]'s planes into one tightly packed buffer.
///
/// `VideoFrame(data, init)` takes a single buffer plus a plane layout, so the
/// planes have to be contiguous. Rows are copied when a plane's stride is wider
/// than its visible width, which is exactly the case WebCodecs wants removed.
Uint8List _tightenPlanes(VideoFrame frame) {
  var total = 0;
  final packedStrides = <int>[];
  for (var index = 0; index < frame.planes.length; index++) {
    final stride = frame.pixelFormat.defaultStride(frame.width, index);
    final rows = frame.pixelFormat.planeHeight(frame.height, index);
    if (stride == null || rows == null) {
      throw AnyCodecException.invalidArgument(
        '${frame.pixelFormat.name} has no plane $index',
        operation: 'encode',
      );
    }
    packedStrides.add(stride);
    total += stride * rows;
  }
  final output = Uint8List(total);
  var cursor = 0;
  for (var index = 0; index < frame.planes.length; index++) {
    final plane = frame.planes[index];
    final sourceStride = frame.strides[index];
    final packedStride = packedStrides[index];
    final rows = frame.pixelFormat.planeHeight(frame.height, index)!;
    final required =
        (sourceStride > packedStride ? sourceStride : packedStride) * rows;
    if (plane.length < required) {
      throw AnyCodecException.invalidArgument(
        'plane $index is ${plane.length} bytes but needs $required',
        operation: 'encode',
      );
    }
    if (sourceStride == packedStride) {
      output.setRange(cursor, cursor + packedStride * rows, plane);
      cursor += packedStride * rows;
      continue;
    }
    for (var row = 0; row < rows; row++) {
      final start = row * sourceStride;
      output.setRange(cursor, cursor + packedStride, plane, start);
      cursor += packedStride;
    }
  }
  return output;
}

/// Reads `metadata.decoderConfig.description` (the `avcC`/`hvcC` blob) out of a
/// WebCodecs chunk metadata object.
///
/// `package:web` 1.1.1 has no type for `EncodedVideoChunkMetadata`, so this uses
/// untyped property access. Any surprise -a missing field, a different shape -/// degrades to `null`, which costs the caller an `extradata()` value and nothing
/// else.
Uint8List? _descriptionFromMetadata(JSAny? metadata) {
  if (metadata == null) {
    return null;
  }
  try {
    final object = metadata as JSObject;
    final decoderConfig = object.getProperty<JSObject?>('decoderConfig'.toJS);
    if (decoderConfig == null) {
      return null;
    }
    final description = decoderConfig.getProperty<JSArrayBuffer?>(
      'description'.toJS,
    );
    if (description == null) {
      return null;
    }
    return description.toDart.asUint8List();
  } catch (_) {
    return null;
  }
}
