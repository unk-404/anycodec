import 'dart:math' as math;
import 'dart:typed_data';

import 'package:anycodec_audio/anycodec_audio.dart';
import 'package:test/test.dart';

/// Fills [frames] of a mono sine at [frequency], amplitude [amplitude].
Float32List _sine({
  required double frequency,
  required int sampleRate,
  required int frames,
  double amplitude = 1.0,
  int startFrame = 0,
}) {
  final out = Float32List(frames);
  for (var i = 0; i < frames; i++) {
    out[i] =
        math.sin(2 * math.pi * frequency * (startFrame + i) / sampleRate) *
        amplitude;
  }
  return out;
}

void main() {
  const sampleRate = 48000;

  group('SpectrumAnalyzer', () {
    test('finds the peak of a tone within one bin', () {
      const fftSize = 4096;
      const tone = 1000.0;
      final analyzer = SpectrumAnalyzer(
        fftSize: fftSize,
        sampleRate: sampleRate,
      );
      analyzer.add(_sine(frequency: tone, sampleRate: sampleRate, frames: fftSize));
      final frame = analyzer.analyze();
      final binHz = sampleRate / fftSize;
      expect(frame.peakFrequency, closeTo(tone, binHz * 1.5));
    });

    test('a full-scale tone reads 0 dBFS after window gain compensation', () {
      const fftSize = 2048;
      final analyzer = SpectrumAnalyzer(
        fftSize: fftSize,
        sampleRate: sampleRate,
      );
      // 1500 Hz is exactly bin 64 at 48 kHz / 2048 points. Level assertions must
      // use an on-bin tone: an off-bin tone loses up to ~1.4 dB to scalloping,
      // which is real physics rather than a defect in the analyser.
      analyzer.add(
        _sine(frequency: 1500, sampleRate: sampleRate, frames: fftSize),
      );
      final frame = analyzer.analyze();
      // Without coherent-gain compensation a Hann window would report about
      // -6 dB here, which on a meter looks like the signal is quiet.
      expect(frame.magnitudesDb[frame.peakBin], closeTo(0.0, 0.2));
    });

    test('half amplitude reads -6 dBFS', () {
      const fftSize = 2048;
      final analyzer = SpectrumAnalyzer(
        fftSize: fftSize,
        sampleRate: sampleRate,
      );
      analyzer.add(
        _sine(
          frequency: 1500,
          sampleRate: sampleRate,
          frames: fftSize,
          amplitude: 0.5,
        ),
      );
      final frame = analyzer.analyze();
      expect(frame.magnitudesDb[frame.peakBin], closeTo(-6.02, 0.2));
    });

    test('every window agrees on the peak level', () {
      const fftSize = 2048;
      final levels = <double>[];
      for (final window in SpectrumWindow.values) {
        final analyzer = SpectrumAnalyzer(
          fftSize: fftSize,
          sampleRate: sampleRate,
          window: window,
        );
        analyzer.add(
          _sine(frequency: 1500, sampleRate: sampleRate, frames: fftSize),
        );
        final frame = analyzer.analyze();
        levels.add(frame.magnitudesDb[frame.peakBin]);
      }
      for (final level in levels) {
        expect(level, closeTo(0.0, 0.3), reason: 'levels: $levels');
      }
    });

    test('the loudest band is the one containing the tone', () {
      const fftSize = 4096;
      const tone = 1000.0;
      final analyzer = SpectrumAnalyzer(
        fftSize: fftSize,
        sampleRate: sampleRate,
        bands: 32,
      );
      analyzer.add(
        _sine(frequency: tone, sampleRate: sampleRate, frames: fftSize),
      );
      final frame = analyzer.analyze();
      expect(frame.bandValues.length, 32);
      for (final value in frame.bandValues) {
        expect(value, inInclusiveRange(0.0, 1.0));
      }
      var loudest = 0;
      for (var band = 1; band < frame.bandValues.length; band++) {
        if (frame.bandValues[band] > frame.bandValues[loudest]) {
          loudest = band;
        }
      }
      final edges = frame.bandEdgesHz;
      expect(
        tone,
        inInclusiveRange(edges[loudest], edges[loudest + 1]),
        reason: 'tone $tone Hz should be inside band $loudest '
            '(${edges[loudest]}..${edges[loudest + 1]})',
      );
    });

    test('band edges ascend logarithmically from min to max', () {
      final analyzer = SpectrumAnalyzer(
        fftSize: 2048,
        sampleRate: sampleRate,
        bands: 16,
        minFrequency: 20,
        maxFrequency: 20000,
      );
      final edges = analyzer.bandEdgesHz;
      expect(edges.length, 17);
      expect(edges.first, closeTo(20, 1e-9));
      expect(edges.last, closeTo(20000, 1e-6));
      for (var i = 1; i < edges.length; i++) {
        expect(edges[i], greaterThan(edges[i - 1]));
      }
    });

    test('smoothing makes a band move less on the first frame', () {
      const fftSize = 2048;
      final raw = SpectrumAnalyzer(fftSize: fftSize, sampleRate: sampleRate);
      final smoothed = SpectrumAnalyzer(
        fftSize: fftSize,
        sampleRate: sampleRate,
        smoothing: 0.9,
      );
      final tone = _sine(frequency: 1000, sampleRate: sampleRate, frames: fftSize);
      raw.add(tone);
      smoothed.add(tone);
      final rawPeak = raw.analyze().bandValues.reduce(math.max);
      final smoothedPeak = smoothed.analyze().bandValues.reduce(math.max);
      expect(smoothedPeak, lessThan(rawPeak));
      expect(smoothedPeak, closeTo(rawPeak * 0.1, 0.15));
    });

    test('multichannel input is mixed to mono, not truncated', () {
      const fftSize = 1024;
      final analyzer = SpectrumAnalyzer(
        fftSize: fftSize,
        sampleRate: sampleRate,
      );
      // Same tone in both channels: mixing must keep the level, not halve it.
      final left = _sine(frequency: 1500, sampleRate: sampleRate, frames: fftSize);
      final interleaved = Float32List(fftSize * 2);
      for (var i = 0; i < fftSize; i++) {
        interleaved[i * 2] = left[i];
        interleaved[i * 2 + 1] = left[i];
      }
      analyzer.add(interleaved, channels: 2);
      final frame = analyzer.analyze();
      expect(frame.magnitudesDb[frame.peakBin], closeTo(0.0, 0.3));
    });

    test('reset forgets the window', () {
      final analyzer = SpectrumAnalyzer(
        fftSize: 1024,
        sampleRate: sampleRate,
      );
      analyzer.add(_sine(frequency: 1000, sampleRate: sampleRate, frames: 512));
      expect(analyzer.filled, 512);
      analyzer.reset();
      expect(analyzer.filled, 0);
      final quiet = analyzer.analyze();
      expect(
        quiet.magnitudesDb.every((db) => db <= SpectrumFrame.floorDb + 1e-9),
        isTrue,
      );
    });

    test('rejects impossible geometry', () {
      expect(
        () => SpectrumAnalyzer(fftSize: 1000, sampleRate: sampleRate),
        throwsArgumentError,
      );
      expect(
        () => SpectrumAnalyzer(fftSize: 1024, sampleRate: sampleRate, bands: 0),
        throwsArgumentError,
      );
      expect(
        () => SpectrumAnalyzer(
          fftSize: 1024,
          sampleRate: sampleRate,
          minFrequency: 20000,
          maxFrequency: 20,
        ),
        throwsArgumentError,
      );
    });
  });
}
