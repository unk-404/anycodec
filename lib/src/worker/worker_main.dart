/// The worker isolate: the only place in the package where native handles live.
///
/// One isolate owns a set of `AnyCodecEncoder` / `AnyCodecDecoder` handles for
/// its whole life. That is not an optimisation, it is a correctness
/// requirement: the C ABI's handles are thread-affine, and every native call
/// here happens on the single thread that created the handle. It is also what
/// keeps the UI isolate responsive — no native call ever runs on it.
///
/// Requests are handled **synchronously**, one at a time, in arrival order.
/// There are no `await`s in the loop, so two requests for the same session can
/// never interleave, and responses come back in the order they were requested.
library;

import 'dart:isolate';

import '../native/native_bindings.dart';
import '../native/status.dart';
import 'protocol.dart';

/// The entry point of every worker isolate.
///
/// Must be a top-level function with a sendable argument for
/// `Isolate.spawn`. It sends a [WorkerHello] once the native library has been
/// verified, then answers [WorkerRequest]s until [WorkerOp.dispose] arrives.
///
/// If the ABI check fails the isolate throws before the hello, which the pool
/// surfaces as a failed `initialize` rather than as a mysterious hang.
@pragma('vm:entry-point')
void anycodecWorkerMain(WorkerBoot boot) {
  NativeBindings.ensureCompatibleAbi();

  final requests = ReceivePort();
  final encoders = <int, NativeEncoderHandle>{};
  final decoders = <int, NativeDecoderHandle>{};

  boot.replyTo.send(
    WorkerHello(workerId: boot.workerId, requestPort: requests.sendPort),
  );

  requests.listen((Object? message) {
    if (message is! WorkerRequest) {
      return;
    }
    Object? value;
    AnyCodecException? failure;
    var dispose = false;
    try {
      value = _handle(message, encoders, decoders);
      dispose = message.op == WorkerOp.dispose;
    } on AnyCodecException catch (error) {
      failure = error;
    } catch (error, stack) {
      failure = AnyCodecException.internal(
        'worker ${boot.workerId} failed while handling ${message.op.name}: '
        '$error\n$stack',
      );
    }
    boot.replyTo.send(
      WorkerResponse(id: message.id, value: value, failure: failure),
    );
    if (dispose) {
      for (final encoder in encoders.values) {
        encoder.close();
      }
      for (final decoder in decoders.values) {
        decoder.close();
      }
      encoders.clear();
      decoders.clear();
      // Closing the last port lets the isolate's event loop drain and the
      // isolate exit on its own; the pool also kills it explicitly.
      requests.close();
    }
  });
}

/// Runs one request to completion on this isolate.
///
/// Deliberately synchronous: the whole point of a worker is that native calls
/// happen on one thread in one order.
Object? _handle(
  WorkerRequest request,
  Map<int, NativeEncoderHandle> encoders,
  Map<int, NativeDecoderHandle> decoders,
) {
  switch (request.op) {
    case WorkerOp.openEncoder:
      final open = request.payload! as OpenEncoderRequest;
      final handle = open.diagnostic
          ? NativeBindings.openDiagnosticEncoder(open.config.codec)
          : NativeBindings.openEncoder(open.config, open.preference);
      encoders[open.sessionId] = handle;
      return OpenedSession(sessionId: open.sessionId, path: handle.path);

    case WorkerOp.openDecoder:
      final open = request.payload! as OpenDecoderRequest;
      final handle = open.diagnostic
          ? NativeBindings.openDiagnosticDecoder(open.config.codec)
          : NativeBindings.openDecoder(open.config, open.preference);
      decoders[open.sessionId] = handle;
      return OpenedSession(sessionId: open.sessionId, path: handle.path);

    case WorkerOp.encode:
      final encode = request.payload! as EncodeRequest;
      final handle = _encoder(encoders, encode.sessionId);
      switch (encode.frame) {
        case VideoFramePayload video:
          handle.pushVideo(video.toFrame());
        case AudioFramePayload audio:
          handle.pushAudio(audio.toFrame());
      }
      return null;

    case WorkerOp.decode:
      final decode = request.payload! as DecodeRequest;
      _decoder(decoders, decode.sessionId).push(decode.packet.toPacket());
      return null;

    case WorkerOp.receive:
      final session = (request.payload! as SessionRequest).sessionId;
      final encoder = encoders[session];
      if (encoder != null) {
        final packet = encoder.receive();
        return packet == null ? null : PacketPayload.fromPacket(packet);
      }
      final frame = _decoder(decoders, session).receive();
      return frame == null ? null : FramePayload.of(frame);

    case WorkerOp.flush:
      final session = (request.payload! as SessionRequest).sessionId;
      final encoder = encoders[session];
      if (encoder != null) {
        return <PacketPayload>[
          for (final packet in encoder.flush())
            PacketPayload.fromPacket(packet),
        ];
      }
      return <FramePayload>[
        for (final frame in _decoder(decoders, session).flush())
          FramePayload.of(frame),
      ];

    case WorkerOp.setBitrate:
      final bitrate = request.payload! as SetBitrateRequest;
      _encoder(encoders, bitrate.sessionId).setBitrate(bitrate.bitsPerSecond);
      return null;

    case WorkerOp.requestKeyframe:
      final session = (request.payload! as SessionRequest).sessionId;
      _encoder(encoders, session).requestKeyframe();
      return null;

    case WorkerOp.extradata:
      final session = (request.payload! as SessionRequest).sessionId;
      return _encoder(encoders, session).extradata();

    case WorkerOp.close:
      final session = (request.payload! as SessionRequest).sessionId;
      encoders.remove(session)?.close();
      decoders.remove(session)?.close();
      return null;

    case WorkerOp.capabilities:
      return NativeBindings.capabilities();

    case WorkerOp.probeHardware:
      return NativeBindings.probeHardware();

    case WorkerOp.capabilityReport:
      return NativeBindings.capabilityReport();

    case WorkerOp.dispose:
      return null;
  }
}

NativeEncoderHandle _encoder(
  Map<int, NativeEncoderHandle> encoders,
  int sessionId,
) {
  final handle = encoders[sessionId];
  if (handle == null) {
    throw AnyCodecException.state(
      'no encoder session $sessionId exists on this worker',
      operation: 'encode',
    );
  }
  return handle;
}

NativeDecoderHandle _decoder(
  Map<int, NativeDecoderHandle> decoders,
  int sessionId,
) {
  final handle = decoders[sessionId];
  if (handle == null) {
    throw AnyCodecException.state(
      'no decoder session $sessionId exists on this worker',
      operation: 'decode',
    );
  }
  return handle;
}
