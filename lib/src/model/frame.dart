/// Raw (uncompressed) video and audio frames.
library;

import 'dart:typed_data';

import 'format.dart';

/// A raw frame handed to an encoder or produced by a decoder.
///
/// Two shapes exist: [VideoFrame] and [AudioFrame]. The sealed hierarchy means a
/// `switch` over a `Frame` is exhaustive, so an encoder can never be handed the
/// wrong kind by accident.
///
/// See `Packet` for the timebase: timestamps are microseconds.
sealed class Frame {
  /// Const constructor for subclasses.
  const Frame();

  /// Presentation timestamp in microseconds.
  int get pts;

  /// Duration in microseconds, or `0` when unknown.
  int get duration;

  /// `true` for a [VideoFrame].
  bool get isVideo => this is VideoFrame;

  /// `true` for an [AudioFrame].
  bool get isAudio => this is AudioFrame;

  /// The frame as a [VideoFrame], or `null` when it is audio.
  VideoFrame? get asVideo => this is VideoFrame ? this as VideoFrame : null;

  /// The frame as an [AudioFrame], or `null` when it is video.
  AudioFrame? get asAudio => this is AudioFrame ? this as AudioFrame : null;

  /// Total number of payload bytes the frame holds across all planes.
  int get byteLength;
}

/// One raw picture.
///
/// [planes] holds one `Uint8List` per plane of [pixelFormat] and [strides] holds
/// the corresponding row stride in bytes. Both lists must be as long as
/// `pixelFormat.planeCount`, and each plane must be at least
/// `pixelFormat.minimumPlaneLength(width, height, index, stride)` bytes long.
/// The Dart layer checks that before it lets native code touch the buffers, so a
/// too-small plane raises an `AnyCodecException` instead of corrupting memory.
final class VideoFrame extends Frame {
  /// Builds a picture.
  ///
  /// Throws [ArgumentError] when the plane or stride counts disagree with
  /// [pixelFormat]; it does *not* check the plane lengths, because a decoder may
  /// legitimately hand back a plane whose stride is larger than the visible
  /// width (the length check belongs at the ABI boundary, where the geometry is
  /// known to be the encoder's).
  VideoFrame({
    required this.pixelFormat,
    required this.width,
    required this.height,
    required List<Uint8List> planes,
    List<int>? strides,
    this.pts = 0,
    this.duration = 0,
    this.keyframe = false,
  }) : planes = List<Uint8List>.unmodifiable(planes),
       strides = List<int>.unmodifiable(
         strides ??
             List<int>.generate(
               planes.length,
               (index) => pixelFormat.defaultStride(width, index) ?? 0,
             ),
       ) {
    if (this.planes.length != pixelFormat.planeCount) {
      throw ArgumentError.value(
        this.planes.length,
        'planes',
        '${pixelFormat.name} carries ${pixelFormat.planeCount} planes',
      );
    }
    if (this.strides.length != this.planes.length) {
      throw ArgumentError.value(
        this.strides.length,
        'strides',
        'expected one stride per plane (${this.planes.length})',
      );
    }
  }

  /// A tightly packed I420 picture built from three plane buffers.
  ///
  /// Convenience for tests and for the example app: it computes the plane
  /// geometry for you. Throws [ArgumentError] when the buffers are too small.
  factory VideoFrame.i420({
    required int width,
    required int height,
    required Uint8List y,
    required Uint8List u,
    required Uint8List v,
    int pts = 0,
    int duration = 0,
    bool keyframe = false,
  }) {
    final format = PixelFormat.i420;
    final lengths = <int>[
      for (var i = 0; i < format.planeCount; i++)
        format.minimumPlaneLength(width, height, i, 0)!,
    ];
    final provided = <Uint8List>[y, u, v];
    for (var i = 0; i < provided.length; i++) {
      if (provided[i].length < lengths[i]) {
        throw ArgumentError.value(
          provided[i].length,
          'plane $i',
          'needs at least ${lengths[i]} bytes for ${width}x$height ${format.name}',
        );
      }
    }
    return VideoFrame(
      pixelFormat: format,
      width: width,
      height: height,
      planes: provided,
      strides: <int>[
        for (var i = 0; i < format.planeCount; i++)
          format.defaultStride(width, i)!,
      ],
      pts: pts,
      duration: duration,
      keyframe: keyframe,
    );
  }

  /// The layout of the picture.
  final PixelFormat pixelFormat;

  /// Luma width in pixels.
  final int width;

  /// Luma height in pixels.
  final int height;

  /// The plane payloads, in ABI order.
  final List<Uint8List> planes;

  /// Row stride in bytes of each plane.
  final List<int> strides;

  @override
  final int pts;

  @override
  final int duration;

  /// `true` when the picture can be decoded standalone.
  final bool keyframe;

  @override
  int get byteLength {
    var total = 0;
    for (final plane in planes) {
      total += plane.length;
    }
    return total;
  }

  /// A copy with every buffer duplicated.
  ///
  /// `AnyCodec` moves video bytes across the isolate boundary rather than
  /// sharing them, so a frame that will be encoded more than once, or by more
  /// than one session, should be cloned per use. (The move happens on the
  /// sending isolate, which copies the bytes into the transport buffer and
  /// leaves this frame's `Uint8List`s intact — see `AnycodecEncoder.encode`.)
  VideoFrame clone({int? pts}) => VideoFrame(
    pixelFormat: pixelFormat,
    width: width,
    height: height,
    planes: <Uint8List>[for (final plane in planes) Uint8List.fromList(plane)],
    strides: strides,
    pts: pts ?? this.pts,
    duration: duration,
    keyframe: keyframe,
  );

  @override
  String toString() =>
      'VideoFrame(${pixelFormat.name} ${width}x$height, '
      '${planes.length} planes, pts=$pts us, keyframe=$keyframe)';
}

/// One block of interleaved raw audio.
final class AudioFrame extends Frame {
  /// Builds a block of audio.
  ///
  /// [frames] is the number of sample frames *per channel*; [data] must hold at
  /// least `channels.byteLength(frames, sampleFormat)` bytes. That is checked
  /// here, so an undersized buffer is an [ArgumentError] rather than a short
  /// read in native code.
  AudioFrame({
    required this.sampleFormat,
    required this.sampleRate,
    required this.channels,
    required this.frames,
    required this.data,
    this.pts = 0,
    this.duration = 0,
  }) {
    final required = channels.byteLength(frames, sampleFormat);
    if (data.length < required) {
      throw ArgumentError.value(
        data.length,
        'data',
        'needs at least $required bytes for $frames frames of '
            '${channels.channels} channel(s) in ${sampleFormat.name}',
      );
    }
  }

  /// How the samples are encoded.
  final SampleFormat sampleFormat;

  /// Samples per second.
  final int sampleRate;

  /// Speaker arrangement.
  final ChannelLayout channels;

  /// Number of sample frames per channel.
  final int frames;

  /// Interleaved sample bytes.
  final Uint8List data;

  @override
  final int pts;

  @override
  final int duration;

  @override
  int get byteLength => data.length;

  /// A copy with the sample buffer duplicated.
  AudioFrame clone({int? pts}) => AudioFrame(
    sampleFormat: sampleFormat,
    sampleRate: sampleRate,
    channels: channels,
    frames: frames,
    data: Uint8List.fromList(data),
    pts: pts ?? this.pts,
    duration: duration,
  );

  @override
  String toString() =>
      'AudioFrame(${sampleFormat.name} ${sampleRate}Hz '
      '${channels.channels}ch, $frames frames, pts=$pts us)';
}
