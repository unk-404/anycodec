import 'dart:math' as math;
import 'dart:typed_data';

import 'package:anycodec_audio/anycodec_audio.dart';
import 'package:test/test.dart';

/// `chunkFrames` stereo frames of a sine, interleaved, as `s16`.
///
/// This is exactly the shape a decoder hands back, and the point of the test is
/// that it is the *only* thing a caller has to produce to drive this package.
Uint8List _interleavedSineS16({
  required double frequency,
  required int sampleRate,
  required int channels,
  required int frames,
  required int startFrame,
  double amplitude = 0.8,
}) {
  final bytes = Uint8List(frames * channels * 2);
  final view = ByteData.sublistView(bytes);
  for (var frame = 0; frame < frames; frame++) {
    final t = (startFrame + frame) / sampleRate;
    final sample = (math.sin(2 * math.pi * frequency * t) * amplitude * 32767)
        .round();
    for (var channel = 0; channel < channels; channel++) {
      view.setInt16((frame * channels + channel) * 2, sample, Endian.little);
    }
  }
  return bytes;
}

void main() {
  test('a complete playback loop can be built from this package alone', () async {
    const sampleRate = 48000;
    const channels = 2;
    const decoderChunkFrames = 1024;
    const deviceChunkFrames = 512;
    const chunksToFeed = 20;
    const toneHz = 1000.0;

    // What the decoder produces (s16 stereo) versus what the device wants to be
    // handed (normalised f32 stereo). The source converts between them so the
    // player loop never touches sample encoding.
    final decoderFormat = PcmFormat(
      sampleFormat: PcmSampleFormat.s16,
      sampleRate: sampleRate,
      channels: channels,
    );
    final deviceFormat = decoderFormat.copyWith(
      sampleFormat: PcmSampleFormat.f32,
    );

    final source = PcmStreamSource(
      format: deviceFormat,
      capacityFrames: decoderChunkFrames * 4,
    );
    final analyzer = SpectrumAnalyzer(
      fftSize: 2048,
      sampleRate: sampleRate,
      bands: 16,
    );

    var framesDelivered = 0;
    var startFrame = 0;

    for (var chunk = 0; chunk < chunksToFeed; chunk++) {
      // ── producer side ─────────────────────────────────────────────────────
      source.addBytes(
        decoderFormat,
        _interleavedSineS16(
          frequency: toneHz,
          sampleRate: sampleRate,
          channels: channels,
          frames: decoderChunkFrames,
          startFrame: startFrame,
        ),
      );
      startFrame += decoderChunkFrames;

      // ── consumer side: this is the loop a player owns ─────────────────────
      for (var block = 0;
          block < decoderChunkFrames ~/ deviceChunkFrames;
          block++) {
        final chunkFrames = await source.readChunk(deviceChunkFrames);
        expect(chunkFrames, isNotNull, reason: 'block $block of chunk $chunk');
        final samples = chunkFrames!;
        expect(samples.length, deviceChunkFrames * channels);
        // A real player would now hand `samples` to its device API. Here it is
        // only analysed, which is all this package claims to offer.
        analyzer.add(samples, channels: channels);
        framesDelivered += deviceChunkFrames;
      }
    }

    expect(framesDelivered, chunksToFeed * decoderChunkFrames);
    expect(source.buffer.droppedFrames, 0, reason: 'the loop kept up');

    final spectrum = analyzer.analyze();
    final binHz = sampleRate / 2048;
    expect(spectrum.peakFrequency, closeTo(toneHz, binHz * 1.5));
    // 0.8 amplitude is about -1.9 dBFS.
    expect(spectrum.magnitudesDb[spectrum.peakBin], closeTo(-1.94, 1.0));
    expect(spectrum.bandValues.reduce(math.max), greaterThan(0.9));

    // Ending the stream is an explicit act, and it ends the loop rather than
    // leaving it spinning on an empty buffer.
    source.close();
    expect(await source.readChunk(deviceChunkFrames), isNull);
  });

  test('an overrunning producer is visible rather than silent', () async {
    final format = PcmFormat(
      sampleFormat: PcmSampleFormat.f32,
      sampleRate: 48000,
      channels: 1,
    );
    // A tight live-style buffer where the producer runs ahead.
    final source = PcmStreamSource(
      format: format,
      capacityFrames: 8,
      dropOldest: true,
    );
    source.addFloat32(Float32List.fromList(List<double>.filled(32, 0.5)));
    expect(
      source.buffer.droppedFrames,
      greaterThan(0),
      reason: 'the defect must be observable, not hidden',
    );
  });
}
