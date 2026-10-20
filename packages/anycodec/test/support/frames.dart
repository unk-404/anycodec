/// Frame builders and drain helpers shared by the test suite.
///
/// Not a test file itself: `dart test` only collects `*_test.dart`, so nothing
/// here runs on its own.
library;

import 'dart:typed_data';

import 'package:anycodec/anycodec.dart';

/// A 16x16 I420 picture whose planes are filled with a distinctive pattern.
VideoFrame sampleVideoFrame({int pts = 0, int width = 16, int height = 16}) {
  final y = Uint8List(width * height);
  for (var index = 0; index < y.length; index++) {
    y[index] = index & 0xff;
  }
  final chroma = (width ~/ 2) * (height ~/ 2);
  final u = Uint8List(chroma);
  final v = Uint8List(chroma);
  for (var index = 0; index < chroma; index++) {
    u[index] = (0x80 + index) & 0xff;
    v[index] = (0x40 + index) & 0xff;
  }
  return VideoFrame.i420(
    width: width,
    height: height,
    y: y,
    u: u,
    v: v,
    pts: pts,
    duration: 1000,
    keyframe: true,
  );
}

/// A small block of interleaved stereo S16.
AudioFrame sampleAudioFrame({int pts = 0, int frames = 256}) {
  final data = Uint8List(
    ChannelLayout.stereo.byteLength(frames, SampleFormat.s16),
  );
  for (var index = 0; index < data.length; index++) {
    data[index] = index & 0xff;
  }
  return AudioFrame(
    sampleFormat: SampleFormat.s16,
    sampleRate: 48000,
    channels: ChannelLayout.stereo,
    frames: frames,
    data: data,
    pts: pts,
    duration: 5333,
  );
}

/// Receives the next packet, giving the session a few polls to produce one.
///
/// `receive` returning `null` means "needs more input", not "finished", so a
/// caller that has already pushed must be prepared to poll.
Future<Packet?> drainPacket(AnycodecEncoder encoder, {int attempts = 4}) async {
  for (var attempt = 0; attempt < attempts; attempt++) {
    final packet = await encoder.receive();
    if (packet != null) {
      return packet;
    }
  }
  return null;
}

/// Receives the next frame, giving the session a few polls to produce one.
Future<Frame?> drainFrame(AnycodecDecoder decoder, {int attempts = 4}) async {
  for (var attempt = 0; attempt < attempts; attempt++) {
    final frame = await decoder.receive();
    if (frame != null) {
      return frame;
    }
  }
  return null;
}
