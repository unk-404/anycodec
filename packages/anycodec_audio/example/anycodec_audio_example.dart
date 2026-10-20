import 'dart:math' as math;
import 'dart:typed_data';

import 'package:anycodec_audio/anycodec_audio.dart';

/// A runnable demonstration of the two things this package does — and of the one
/// thing it refuses to do (play audio).
///
/// Run it with `dart run example/anycodec_audio_example.dart`.
void main() {
  const sampleRate = 48000;
  const channels = 2;
  const fftSize = 2048;

  final format = PcmFormat(
    sampleFormat: PcmSampleFormat.s16,
    sampleRate: sampleRate,
    channels: channels,
  );
  final source = PcmStreamSource(
    format: format.copyWith(sampleFormat: PcmSampleFormat.f32),
  );
  final analyzer = SpectrumAnalyzer(
    fftSize: fftSize,
    sampleRate: sampleRate,
    bands: 32,
  );

  // A 1 kHz tone as a player's decoder would deliver it: interleaved s16.
  const toneHz = 1000.0;
  const chunkFrames = 960;
  final tone = _sineBytes(
    frequency: toneHz,
    sampleRate: sampleRate,
    channels: channels,
    frames: chunkFrames,
    startFrame: 0,
  );
  source.addBytes(format, tone);

  final chunk = source.readChunk(chunkFrames);
  // `readChunk` is a `Future`, but with enough data buffered it completes from a
  // microtask; the point of the example is the shape, not the scheduler.
  chunk.then((samples) {
    if (samples == null) {
      print('source closed');
      return;
    }
    analyzer.add(samples, channels: channels);
    final spectrum = analyzer.analyze();
    print('filled samples: ${analyzer.filled}/$fftSize');
    print('peak: ${spectrum.peakFrequency.toStringAsFixed(1)} Hz '
        '(fed $toneHz Hz)');
    print('peak level: ${spectrum.magnitudesDb[spectrum.peakBin].toStringAsFixed(1)} dBFS');
    print('bands (0..1), loudest first:');
    final order = List<int>.generate(spectrum.bandValues.length, (i) => i)
      ..sort((a, b) => spectrum.bandValues[b].compareTo(spectrum.bandValues[a]));
    for (final band in order.take(3)) {
      final low = spectrum.bandEdgesHz[band];
      final high = spectrum.bandEdgesHz[band + 1];
      print('  band $band  ${low.toStringAsFixed(0)}-${high.toStringAsFixed(0)} Hz'
          '  ${spectrum.bandValues[band].toStringAsFixed(3)}');
    }
    print('');
    print('A player would now hand `samples` to its device API. '
        'This package does not do that, on purpose.');
  });
}

/// Interleaved s16 sine, the encoding most decoders hand back.
Uint8List _sineBytes({
  required double frequency,
  required int sampleRate,
  required int channels,
  required int frames,
  required int startFrame,
}) {
  final out = Uint8List(frames * channels * 2);
  final view = ByteData.sublistView(out);
  for (var frame = 0; frame < frames; frame++) {
    final t = (startFrame + frame) / sampleRate;
    final value = math.sin(2 * math.pi * frequency * t) * 0.5; // -6 dBFS
    final sample = (value * 32767).round();
    for (var channel = 0; channel < channels; channel++) {
      view.setInt16((frame * channels + channel) * 2, sample, Endian.little);
    }
  }
  return out;
}
