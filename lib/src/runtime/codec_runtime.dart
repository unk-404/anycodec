/// The runtime abstraction the unified API in `anycodec.dart` calls.
///
/// Three implementations exist, selected by conditional import:
///
/// * `runtime_io.dart` — the native path: a pool of worker isolates, each
///   owning native handles;
/// * `runtime_web.dart` — the browser path: WebCodecs plus a pure-Dart
///   diagnostic codec, with no isolates (there are none on the web);
/// * `runtime_stub.dart` — any other platform: every call throws a documented
///   `AnyCodecException` rather than failing at link time.
///
/// The interfaces below are what makes the unified API *unified*: they are
/// `Future`-based on every platform, so neither a native session nor a WebCodecs
/// session can block the caller's event loop.
library;

import 'dart:async';
import 'dart:typed_data';

import '../model/capability.dart';
import '../model/codec.dart';
import '../model/config.dart';
import '../model/frame.dart';
import '../model/packet.dart';

/// A live encode session.
///
/// Sessions are bound to one worker isolate (native) or to one WebCodecs
/// encoder (web) for their whole life. They are **not** thread-safe: issue one
/// operation at a time per session.
abstract interface class AnycodecEncoder {
  /// The codec this session encodes.
  Codec get codec;

  /// How this session is doing its work, including whether it fell back to
  /// software and why. Read once at creation; it does not change.
  CodecPath get path;

  /// The worker isolate that owns this session.
  ///
  /// This is `0` on the web runtime, which has no isolates. It exists so a
  /// caller (and the test suite) can prove that concurrent sessions really do
  /// land on different threads.
  int get workerId;

  /// `true` once [close] has run.
  bool get isClosed;

  /// Submits one frame.
  ///
  /// On the native runtime the frame's bytes are moved across the isolate
  /// boundary (the transport copies them once into a private buffer, so the
  /// `Uint8List`s you passed in stay valid, but they are no longer shared with
  /// the worker). Nothing is returned synchronously; drain the session with
  /// [receive].
  ///
  /// Throws `AnyCodecException` when the session is closed, when the frame does
  /// not match the configured codec's media kind, or when the backend rejects
  /// the frame (a plane that is too small is rejected by the Dart layer before
  /// native code can see it).
  Future<void> encode(Frame frame);

  /// Drains one compressed packet.
  ///
  /// Returns `null` when the encoder needs more input. On failure it throws
  /// `AnyCodecException` rather than returning a partial packet.
  Future<Packet?> receive();

  /// Signals end of stream and drains every remaining packet.
  ///
  /// The returned list is in stream order and may be empty. After a flush the
  /// session is still usable: a subsequent [encode] starts a new stream.
  Future<List<Packet>> flush();

  /// How many frames have been accepted but not yet delivered by [receive].
  ///
  /// The C ABI has no query for the backend's internal queue, so this counts the
  /// frames the *caller* has submitted minus the packets it has drained; it is a
  /// lower bound on the work in flight, and resets to zero after [flush].
  Future<int> bufferedPackets();

  /// Asks the encoder to change its target bitrate.
  ///
  /// Throws `AnyCodecException` with `kind == unsupported` on backends that
  /// cannot retune a live session, and with `kind == state` on a closed session.
  Future<void> setBitrate(int bitsPerSecond);

  /// Asks for a keyframe at the next opportunity.
  Future<void> requestKeyframe();

  /// The codec configuration bytes (`avcC`, `hvcC`, `AudioSpecificConfig`, ...),
  /// or `null` when the encoder has not produced any yet.
  Future<Uint8List?> extradata();

  /// Releases the session.
  ///
  /// Safe to call more than once, and safe after a failure: it never throws for
  /// an already-closed session. Every other method throws
  /// `AnyCodecException` with `kind == state` once this has run.
  Future<void> close();
}

/// A live decode session. See [AnycodecEncoder] for the shared contract.
abstract interface class AnycodecDecoder {
  /// The codec this session decodes.
  Codec get codec;

  /// How this session is doing its work.
  CodecPath get path;

  /// The worker isolate that owns this session (`0` on the web runtime).
  int get workerId;

  /// `true` once [close] has run.
  bool get isClosed;

  /// Submits one compressed packet.
  ///
  /// Header packets and end-of-stream markers are submitted through this same
  /// method; set `PacketFlags.header` / `PacketFlags.eos` accordingly.
  Future<void> decode(Packet packet);

  /// Drains one decoded frame, or `null` when more input is required.
  Future<Frame?> receive();

  /// Signals end of stream and drains every remaining frame.
  Future<List<Frame>> flush();

  /// Releases the session. Idempotent; never throws for an already-closed
  /// session.
  Future<void> close();
}

/// A platform runtime: the object the unified `AnyCodec` class delegates to.
abstract interface class CodecRuntime {
  /// The worker count this runtime is using, or `0` before the first use.
  int get workerCount;

  /// The codec slot the diagnostic codec advertises, for logging.
  int get passthroughCodecId;

  /// Prepares the runtime.
  ///
  /// `workers == 0` means "auto": on the native runtime that is
  /// `max(1, cpuCount - 1)`. Calling this again with the count already in use,
  /// or with `0`, is a no-op; changing the count while sessions are open throws
  /// `AnyCodecException` with `kind == state`.
  Future<void> initialize({int workers = 0});

  /// Releases every open session and every worker.
  ///
  /// Idempotent. The runtime is usable again afterwards: the next operation
  /// re-initialises it.
  Future<void> shutdown();

  /// The capability report for all twelve codecs, in both directions.
  Future<List<CodecCapability>> capabilities();

  /// Probes the machine's hardware media engines.
  ///
  /// On the native runtime this always runs on a worker isolate: the C ABI
  /// documents the call as potentially slow and GPU-touching.
  Future<List<HardwareDevice>> probeHardware();

  /// A human readable rendering of the capability report.
  Future<String> capabilityReport();

  /// A human readable report of the *platform specific* extras.
  ///
  /// On native that is the loaded library (ABI revision, version, asset id,
  /// best-effort path), the thread-local last error, the diagnostic codec id and
  /// the worker count; on the web it is WebCodecs support.
  ///
  /// It exists so that UI code — which must compile for every platform — can
  /// show the platform specifics without naming a conditionally exported symbol.
  /// The *typed* equivalents live in `package:anycodec/anycodec_platform.dart`
  /// (`NativeLibraryInfo`, `LastNativeError`, `WebCodecs`); this is their text
  /// rendering.
  Future<String> platformReport();

  /// Creates a production encoder.
  Future<AnycodecEncoder> encoder(
    EncoderConfig config, {
    BackendPreference preference = BackendPreference.auto,
  });

  /// Creates a production decoder.
  Future<AnycodecDecoder> decoder(
    DecoderConfig config, {
    BackendPreference preference = BackendPreference.auto,
  });

  /// Creates the diagnostic store encoder, which needs no platform support.
  Future<AnycodecEncoder> diagnosticEncoder(Codec codec);

  /// Creates the diagnostic retrieve decoder.
  Future<AnycodecDecoder> diagnosticDecoder(Codec codec);
}
