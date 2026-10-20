/// The message protocol between the main isolate and a worker isolate.
///
/// Every message is a plain immutable object with sendable fields. Bytes cross
/// the boundary as [TransferableTypedData] rather than as `Uint8List`: the
/// transport copies the bytes once into a private buffer that the *receiving*
/// isolate then takes ownership of, so delivery itself costs nothing and the two
/// isolates can never observe the same mutable buffer.
///
/// `TransferableTypedData` is single-use -[VideoFramePayload.toFrame] and
/// friends call `materialize()`, which must happen exactly once per payload.
library;

import 'dart:isolate';
import 'dart:typed_data';

import '../model/capability.dart';
import '../model/codec.dart';
import '../model/config.dart';
import '../model/frame.dart';
import '../model/format.dart';
import '../model/packet.dart';
import '../model/error.dart';

/// What a [WorkerRequest] asks the worker to do.
enum WorkerOp {
  /// Creates a native encoder handle.
  openEncoder,

  /// Creates a native decoder handle.
  openDecoder,

  /// Submits one frame to an encoder.
  encode,

  /// Submits one packet to a decoder.
  decode,

  /// Drains one packet or frame from a session.
  receive,

  /// Signals end of stream and drains the tail.
  flush,

  /// Changes an encoder's target bitrate.
  setBitrate,

  /// Asks an encoder for a keyframe.
  requestKeyframe,

  /// Reads an encoder's codec configuration bytes.
  extradata,

  /// Releases one session.
  close,

  /// Reads the capability report.
  capabilities,

  /// Probes the hardware media engines.
  probeHardware,

  /// Reads the human readable capability report.
  capabilityReport,

  /// Releases every session this worker owns.
  dispose,
}

/// Sent once, by the main isolate, to start a worker.
final class WorkerBoot {
  /// Builds the boot message.
  const WorkerBoot({required this.workerId, required this.replyTo});

  /// The worker's index in the pool, `0`-based.
  final int workerId;

  /// Where the worker sends its [WorkerHello] and its responses.
  final SendPort replyTo;
}

/// Sent once, by the worker, once it is ready to accept requests.
final class WorkerHello {
  /// Builds the hello message.
  const WorkerHello({required this.workerId, required this.requestPort});

  /// The worker's index, echoed back so the pool can correlate it.
  final int workerId;

  /// The port the pool sends [WorkerRequest]s to.
  final SendPort requestPort;
}

/// One request from the pool to a worker.
final class WorkerRequest {
  /// Builds a request.
  const WorkerRequest({required this.id, required this.op, this.payload});

  /// Per-worker request id, echoed in the [WorkerResponse].
  final int id;

  /// What to do.
  final WorkerOp op;

  /// The op's argument, if any.
  final Object? payload;
}

/// One response from a worker to the pool.
final class WorkerResponse {
  /// Builds a response.
  const WorkerResponse({required this.id, this.value, this.failure});

  /// The id of the [WorkerRequest] this answers.
  final int id;

  /// The result, or `null` for a void op.
  final Object? value;

  /// The failure, when the op threw. Exactly one of [value] and [failure] is
  /// meaningful.
  final AnyCodecException? failure;
}

/// The result of an `openEncoder` / `openDecoder` request.
final class OpenedSession {
  /// Builds the result.
  const OpenedSession({required this.sessionId, required this.path});

  /// The session id the pool assigned.
  final int sessionId;

  /// How the new session is doing its work.
  final CodecPath path;
}

/// An `openEncoder` request.
final class OpenEncoderRequest {
  /// Builds the request.
  const OpenEncoderRequest({
    required this.sessionId,
    required this.config,
    required this.preference,
    required this.diagnostic,
  });

  /// The id the pool assigned to this session.
  final int sessionId;

  /// What to encode.
  final EncoderConfig config;

  /// Which backends are acceptable.
  final BackendPreference preference;

  /// `true` to open the diagnostic codec instead of the production registry.
  final bool diagnostic;
}

/// An `openDecoder` request.
final class OpenDecoderRequest {
  /// Builds the request.
  const OpenDecoderRequest({
    required this.sessionId,
    required this.config,
    required this.preference,
    required this.diagnostic,
  });

  /// The id the pool assigned to this session.
  final int sessionId;

  /// What to decode.
  final DecoderConfig config;

  /// Which backends are acceptable.
  final BackendPreference preference;

  /// `true` to open the diagnostic codec instead of the production registry.
  final bool diagnostic;
}

/// An `encode` request.
final class EncodeRequest {
  /// Builds the request.
  const EncodeRequest({required this.sessionId, required this.frame});

  /// The target session.
  final int sessionId;

  /// The frame, with its bytes already detached into the transport.
  final FramePayload frame;
}

/// A `decode` request.
final class DecodeRequest {
  /// Builds the request.
  const DecodeRequest({required this.sessionId, required this.packet});

  /// The target session.
  final int sessionId;

  /// The packet, with its bytes already detached into the transport.
  final PacketPayload packet;
}

/// A request that only names a session.
final class SessionRequest {
  /// Builds the request.
  const SessionRequest(this.sessionId);

  /// The target session.
  final int sessionId;
}

/// A `setBitrate` request.
final class SetBitrateRequest {
  /// Builds the request.
  const SetBitrateRequest({
    required this.sessionId,
    required this.bitsPerSecond,
  });

  /// The target session.
  final int sessionId;

  /// The new target bitrate in bits per second.
  final int bitsPerSecond;
}

/// A frame on the wire.
///
/// Convert with [FramePayload.of] on the sending isolate and [toFrame] on the
/// receiving one.
sealed class FramePayload {
  /// Const constructor for subclasses.
  const FramePayload();

  /// Detaches [frame]'s bytes into a transport payload.
  ///
  /// The frame's own `Uint8List`s stay readable on the sending isolate; what
  /// moves is the private copy the transport made.
  factory FramePayload.of(Frame frame) => switch (frame) {
    VideoFrame video => VideoFramePayload.fromFrame(video),
    AudioFrame audio => AudioFramePayload.fromFrame(audio),
  };

  /// Rebuilds the frame on the receiving isolate.
  ///
  /// Each underlying `TransferableTypedData` is materialised, so this must run
  /// at most once per payload.
  Frame toFrame();
}

/// A picture on the wire.
final class VideoFramePayload extends FramePayload {
  /// Builds a payload.
  const VideoFramePayload({
    required this.pixelFormat,
    required this.width,
    required this.height,
    required this.planes,
    required this.strides,
    required this.pts,
    required this.duration,
    required this.keyframe,
  });

  /// Detaches a [VideoFrame]'s planes.
  factory VideoFramePayload.fromFrame(VideoFrame frame) => VideoFramePayload(
    pixelFormat: frame.pixelFormat,
    width: frame.width,
    height: frame.height,
    planes: <TransferableTypedData>[
      for (final plane in frame.planes)
        TransferableTypedData.fromList(<TypedData>[plane]),
    ],
    strides: frame.strides,
    pts: frame.pts,
    duration: frame.duration,
    keyframe: frame.keyframe,
  );

  /// The picture layout.
  final PixelFormat pixelFormat;

  /// Luma width in pixels.
  final int width;

  /// Luma height in pixels.
  final int height;

  /// The plane bytes, as transferable buffers.
  final List<TransferableTypedData> planes;

  /// Row stride in bytes of each plane.
  final List<int> strides;

  /// Presentation timestamp in microseconds.
  final int pts;

  /// Duration in microseconds.
  final int duration;

  /// `true` when the picture can be decoded standalone.
  final bool keyframe;

  @override
  VideoFrame toFrame() => VideoFrame(
    pixelFormat: pixelFormat,
    width: width,
    height: height,
    planes: <Uint8List>[
      for (final plane in planes) Uint8List.view(plane.materialize()),
    ],
    strides: strides,
    pts: pts,
    duration: duration,
    keyframe: keyframe,
  );
}

/// A block of audio on the wire.
final class AudioFramePayload extends FramePayload {
  /// Builds a payload.
  const AudioFramePayload({
    required this.sampleFormat,
    required this.sampleRate,
    required this.channels,
    required this.frames,
    required this.data,
    required this.pts,
    required this.duration,
  });

  /// Detaches an [AudioFrame]'s samples.
  factory AudioFramePayload.fromFrame(AudioFrame frame) => AudioFramePayload(
    sampleFormat: frame.sampleFormat,
    sampleRate: frame.sampleRate,
    channels: frame.channels,
    frames: frame.frames,
    data: TransferableTypedData.fromList(<TypedData>[frame.data]),
    pts: frame.pts,
    duration: frame.duration,
  );

  /// How the samples are encoded.
  final SampleFormat sampleFormat;

  /// Samples per second.
  final int sampleRate;

  /// Speaker arrangement.
  final ChannelLayout channels;

  /// Number of sample frames per channel.
  final int frames;

  /// The interleaved samples, as a transferable buffer.
  final TransferableTypedData data;

  /// Presentation timestamp in microseconds.
  final int pts;

  /// Duration in microseconds.
  final int duration;

  @override
  AudioFrame toFrame() => AudioFrame(
    sampleFormat: sampleFormat,
    sampleRate: sampleRate,
    channels: channels,
    frames: frames,
    data: Uint8List.view(data.materialize()),
    pts: pts,
    duration: duration,
  );
}

/// A compressed packet on the wire.
final class PacketPayload {
  /// Builds a payload.
  const PacketPayload({
    required this.codec,
    required this.data,
    required this.pts,
    required this.dts,
    required this.duration,
    required this.flags,
  });

  /// Detaches a [Packet]'s bytes.
  factory PacketPayload.fromPacket(Packet packet) => PacketPayload(
    codec: packet.codec,
    data: TransferableTypedData.fromList(<TypedData>[packet.data]),
    pts: packet.pts,
    dts: packet.dts,
    duration: packet.duration,
    flags: packet.flags.bits,
  );

  /// The codec the payload belongs to.
  final Codec codec;

  /// The compressed bytes, as a transferable buffer.
  final TransferableTypedData data;

  /// Presentation timestamp in microseconds.
  final int pts;

  /// Decode timestamp in microseconds.
  final int dts;

  /// Duration in microseconds, or `0` when unknown.
  final int duration;

  /// The raw `ANYCODEC_PACKET_FLAG_*` bitmask.
  final int flags;

  /// Rebuilds the packet on the receiving isolate.
  ///
  /// Must run at most once per payload.
  Packet toPacket() => Packet(
    codec: codec,
    data: Uint8List.view(data.materialize()),
    pts: pts,
    dts: dts,
    duration: duration,
    flags: PacketFlags(flags),
  );
}
