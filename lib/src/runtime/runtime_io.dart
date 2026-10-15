/// The native runtime: a worker pool plus the FFI bindings.
///
/// This is the only file the unified API selects on a platform with `dart:io`.
/// Nothing here ever runs a native call on the calling isolate; every operation
/// is marshalled to the worker that owns the session.
library;

import 'dart:async';
import 'dart:typed_data';

import '../model/capability.dart';
import '../model/codec.dart';
import '../model/config.dart';
import '../model/frame.dart';
import '../model/packet.dart';
import '../native/native_bindings.dart';
import '../native/status.dart';
import '../platform/platform_io.dart';
import '../worker/pool.dart';
import '../worker/protocol.dart';
import 'codec_runtime.dart';

/// Creates the native runtime.
///
/// Selected by `lib/anycodec.dart` through a conditional import; the entry point
/// name and signature are shared with `runtime_web.dart` and
/// `runtime_stub.dart`.
CodecRuntime createRuntime() => IoCodecRuntime();

/// The `dart:io` runtime.
final class IoCodecRuntime implements CodecRuntime {
  final WorkerPool _pool = WorkerPool();
  final Map<int, _IoSession> _sessions = <int, _IoSession>{};
  int _nextSessionId = 1;

  @override
  int get workerCount => _pool.workerCount;

  @override
  int get passthroughCodecId => NativeBindings.passthroughCodecId();

  @override
  Future<void> initialize({int workers = 0}) async {
    if (_pool.isInitialized) {
      if (workers == 0 || workers == _pool.workerCount) {
        return;
      }
      if (_sessions.isNotEmpty) {
        throw AnyCodecException.state(
          'cannot change the worker count while ${_sessions.length} session(s) '
          'are open; close them or call AnyCodec.shutdown() first',
        );
      }
      await _pool.shutdown();
    }
    await _pool.initialize(workers: workers);
  }

  @override
  Future<void> shutdown() async {
    final sessions = _sessions.values.toList(growable: false);
    _sessions.clear();
    for (final session in sessions) {
      await session.close();
    }
    await _pool.shutdown();
  }

  @override
  Future<List<CodecCapability>> capabilities() async {
    final value = await _report(WorkerOp.capabilities);
    return value! as List<CodecCapability>;
  }

  @override
  Future<List<HardwareDevice>> probeHardware() async {
    final value = await _report(WorkerOp.probeHardware);
    return value! as List<HardwareDevice>;
  }

  @override
  Future<String> capabilityReport() async {
    final value = await _report(WorkerOp.capabilityReport);
    return value! as String;
  }

  @override
  Future<String> platformReport() async {
    final buffer = StringBuffer()
      ..writeln('native runtime (dart:io)')
      ..writeln('------------------------');
    try {
      final info = NativeLibraryInfo.read();
      buffer
        ..writeln('ABI revision        : ${info.abiVersion}')
        ..writeln(
          'bindings expect     : abi ${info.expectedAbiVersion} '
          '(${info.abiMatches ? 'match' : 'MISMATCH'})',
        )
        ..writeln('library version     : ${info.versionString}')
        ..writeln('native asset        : ${info.assetId}')
        ..writeln(
          'library path        : '
          '${info.libraryPath ?? '(not exposed by the native-asset toolchain)'}',
        );
    } on AnyCodecException catch (error) {
      buffer.writeln('native library      : unavailable: ${error.message}');
    }
    buffer
      ..writeln('diagnostic codec id : $passthroughCodecId (internal slot)')
      ..writeln('worker isolates     : ${_pool.workerCount}');
    final lastError = LastNativeError.read();
    buffer.writeln(
      'last native error   : '
      '${lastError.isOk ? 'none (status 0)' : '${lastError.kind.token} '
                '(${lastError.status}): ${lastError.message}'}',
    );
    return buffer.toString();
  }

  @override
  Future<AnycodecEncoder> encoder(
    EncoderConfig config, {
    BackendPreference preference = BackendPreference.auto,
  }) async {
    final worker = await _takeWorker();
    return _openEncoder(worker, config, preference, diagnostic: false);
  }

  @override
  Future<AnycodecDecoder> decoder(
    DecoderConfig config, {
    BackendPreference preference = BackendPreference.auto,
  }) async {
    final worker = await _takeWorker();
    return _openDecoder(worker, config, preference, diagnostic: false);
  }

  @override
  Future<AnycodecEncoder> diagnosticEncoder(Codec codec) async {
    final worker = await _takeWorker();
    return _openEncoder(
      worker,
      EncoderConfig(codec: codec),
      BackendPreference.auto,
      diagnostic: true,
    );
  }

  @override
  Future<AnycodecDecoder> diagnosticDecoder(Codec codec) async {
    final worker = await _takeWorker();
    return _openDecoder(
      worker,
      DecoderConfig(codec: codec),
      BackendPreference.auto,
      diagnostic: true,
    );
  }

  Future<Object?> _report(WorkerOp op) async {
    await _pool.ensureInitialized();
    return _pool.nextWorker().send(op);
  }

  Future<WorkerHandle> _takeWorker() async {
    await _pool.ensureInitialized();
    return _pool.nextWorker();
  }

  Future<AnycodecEncoder> _openEncoder(
    WorkerHandle worker,
    EncoderConfig config,
    BackendPreference preference, {
    required bool diagnostic,
  }) async {
    final sessionId = _nextSessionId++;
    final value = await worker.send(
      WorkerOp.openEncoder,
      OpenEncoderRequest(
        sessionId: sessionId,
        config: config,
        preference: preference,
        diagnostic: diagnostic,
      ),
    );
    final opened = value! as OpenedSession;
    final session = _IoEncoder(
      this,
      sessionId,
      worker,
      config.codec,
      opened.path,
      diagnostic,
    );
    _sessions[sessionId] = session;
    return session;
  }

  Future<AnycodecDecoder> _openDecoder(
    WorkerHandle worker,
    DecoderConfig config,
    BackendPreference preference, {
    required bool diagnostic,
  }) async {
    final sessionId = _nextSessionId++;
    final value = await worker.send(
      WorkerOp.openDecoder,
      OpenDecoderRequest(
        sessionId: sessionId,
        config: config,
        preference: preference,
        diagnostic: diagnostic,
      ),
    );
    final opened = value! as OpenedSession;
    final session = _IoDecoder(
      this,
      sessionId,
      worker,
      config.codec,
      opened.path,
      diagnostic,
    );
    _sessions[sessionId] = session;
    return session;
  }

  void _forget(int sessionId) => _sessions.remove(sessionId);
}

/// Shared bookkeeping for the two native session shapes.
sealed class _IoSession {
  _IoSession(
    this.runtime,
    this.id,
    this.worker,
    this.codec,
    this.path,
    this.diagnostic,
  );

  final IoCodecRuntime runtime;
  final int id;
  final WorkerHandle worker;
  final Codec codec;
  final CodecPath path;
  final bool diagnostic;
  bool _closed = false;

  /// `true` once [close] has run.
  bool get isClosed => _closed;

  void _requireOpen(String operation) {
    if (_closed) {
      throw AnyCodecException.state(
        'this session has been closed',
        operation: operation,
      );
    }
  }

  /// Releases the native handle on its worker and forgets the session.
  ///
  /// Never throws: closing is how a caller recovers from an earlier failure, so
  /// a dead worker must not turn it into a second failure. The handle cannot
  /// leak either way — it lives in that worker.
  Future<void> close() async {
    if (_closed) {
      return;
    }
    _closed = true;
    runtime._forget(id);
    if (worker.isAlive) {
      try {
        await worker.send(WorkerOp.close, SessionRequest(id));
      } on AnyCodecException {
        // The worker died; its handles died with it.
      }
    }
  }
}

/// One native encode session, bound to one worker isolate.
final class _IoEncoder extends _IoSession implements AnycodecEncoder {
  _IoEncoder(
    super.runtime,
    super.id,
    super.worker,
    super.codec,
    super.path,
    super.diagnostic,
  );

  int _outstanding = 0;

  @override
  int get workerId => worker.id;

  @override
  Future<void> encode(Frame frame) async {
    _requireOpen('encode');
    if (!diagnostic && frame.isVideo != codec.isVideo) {
      throw AnyCodecException.invalidArgument(
        'this session encodes ${codec.isVideo ? 'pictures' : 'sound'} but was '
        'handed a ${frame.isVideo ? 'VideoFrame' : 'AudioFrame'}',
        operation: 'encode',
      );
    }
    await worker.send(
      WorkerOp.encode,
      EncodeRequest(sessionId: id, frame: FramePayload.of(frame)),
    );
    _outstanding++;
  }

  @override
  Future<Packet?> receive() async {
    _requireOpen('receive');
    final value = await worker.send(WorkerOp.receive, SessionRequest(id));
    if (value == null) {
      return null;
    }
    if (value is! PacketPayload) {
      throw AnyCodecException.internal(
        'the encode worker answered a receive with ${value.runtimeType}',
        operation: 'receive',
      );
    }
    if (_outstanding > 0) {
      _outstanding--;
    }
    return value.toPacket();
  }

  @override
  Future<List<Packet>> flush() async {
    _requireOpen('flush');
    final value = await worker.send(WorkerOp.flush, SessionRequest(id));
    if (value is! List) {
      throw AnyCodecException.internal(
        'the encode worker answered a flush with ${value.runtimeType}',
        operation: 'flush',
      );
    }
    _outstanding = 0;
    return <Packet>[
      for (final payload in value) (payload as PacketPayload).toPacket(),
    ];
  }

  @override
  Future<int> bufferedPackets() async => _outstanding;

  @override
  Future<void> setBitrate(int bitsPerSecond) async {
    _requireOpen('setBitrate');
    await worker.send(
      WorkerOp.setBitrate,
      SetBitrateRequest(sessionId: id, bitsPerSecond: bitsPerSecond),
    );
  }

  @override
  Future<void> requestKeyframe() async {
    _requireOpen('requestKeyframe');
    await worker.send(WorkerOp.requestKeyframe, SessionRequest(id));
  }

  @override
  Future<Uint8List?> extradata() async {
    _requireOpen('extradata');
    final value = await worker.send(WorkerOp.extradata, SessionRequest(id));
    if (value == null) {
      return null;
    }
    return value as Uint8List;
  }
}

/// One native decode session, bound to one worker isolate.
final class _IoDecoder extends _IoSession implements AnycodecDecoder {
  _IoDecoder(
    super.runtime,
    super.id,
    super.worker,
    super.codec,
    super.path,
    super.diagnostic,
  );

  @override
  int get workerId => worker.id;

  @override
  Future<void> decode(Packet packet) async {
    _requireOpen('decode');
    if (packet.codec != codec && !diagnostic) {
      throw AnyCodecException.invalidArgument(
        'this session decodes ${codec.name} but was handed a '
        '${packet.codec.name} packet',
        operation: 'decode',
      );
    }
    await worker.send(
      WorkerOp.decode,
      DecodeRequest(sessionId: id, packet: PacketPayload.fromPacket(packet)),
    );
  }

  @override
  Future<Frame?> receive() async {
    _requireOpen('receive');
    final value = await worker.send(WorkerOp.receive, SessionRequest(id));
    if (value == null) {
      return null;
    }
    if (value is! FramePayload) {
      throw AnyCodecException.internal(
        'the decode worker answered a receive with ${value.runtimeType}',
        operation: 'receive',
      );
    }
    return value.toFrame();
  }

  @override
  Future<List<Frame>> flush() async {
    _requireOpen('flush');
    final value = await worker.send(WorkerOp.flush, SessionRequest(id));
    if (value is! List) {
      throw AnyCodecException.internal(
        'the decode worker answered a flush with ${value.runtimeType}',
        operation: 'flush',
      );
    }
    return <Frame>[
      for (final payload in value) (payload as FramePayload).toFrame(),
    ];
  }
}
