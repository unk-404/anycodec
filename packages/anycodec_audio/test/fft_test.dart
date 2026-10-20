import 'dart:math' as math;
import 'dart:typed_data';

import 'package:anycodec_audio/anycodec_audio.dart';
import 'package:test/test.dart';

void main() {
  group('FftPlan', () {
    test('rejects sizes that are not a power of two', () {
      expect(() => FftPlan(1000), throwsArgumentError);
      expect(() => FftPlan(1), throwsArgumentError);
      expect(() => FftPlan(0), throwsArgumentError);
    });

    test('transforms the sizes a visualiser actually uses', () {
      for (final size in [64, 256, 1024, 2048, 4096]) {
        expect(() => FftPlan(size), returnsNormally, reason: 'size $size');
      }
    });

    test('a sine lands in exactly one bin, at unit magnitude', () {
      const size = 1024;
      const bin = 64;
      final real = Float64List(size);
      for (var i = 0; i < size; i++) {
        real[i] = math.sin(2 * math.pi * bin * i / size);
      }
      final magnitudes = FftPlan(size).magnitudesOf(real);
      expect(magnitudes[bin], closeTo(1.0, 1e-9));
      for (var k = 0; k < magnitudes.length; k++) {
        if (k == bin) {
          continue;
        }
        expect(
          magnitudes[k],
          lessThan(1e-9),
          reason: 'bin $k should be empty for an on-bin sine',
        );
      }
    });

    test('DC is not doubled', () {
      const size = 128;
      final real = Float64List(size)..fillRange(0, size, 1.0);
      final magnitudes = FftPlan(size).magnitudesOf(real);
      // A constant 1.0 is full scale at DC. With the mirroring formula wrongly
      // applied to bin 0 this would read 2.0 (+6 dB).
      expect(magnitudes[0], closeTo(1.0, 1e-12));
      expect(magnitudes[1], lessThan(1e-12));
    });

    test('the Nyquist bin is not doubled either', () {
      const size = 64;
      final real = Float64List(size);
      for (var i = 0; i < size; i++) {
        // Alternating +1/-1 is exactly Nyquist.
        real[i] = i.isEven ? 1.0 : -1.0;
      }
      final magnitudes = FftPlan(size).magnitudesOf(real);
      expect(magnitudes[magnitudes.length - 1], closeTo(1.0, 1e-12));
    });

    test('binFrequency maps bins to Hz', () {
      final plan = FftPlan(1024);
      expect(plan.binFrequency(0, 48000), 0);
      expect(plan.binFrequency(512, 48000), closeTo(24000, 1e-9));
      expect(plan.binFrequency(1, 48000), closeTo(46.875, 1e-9));
    });

    test('forward then inverse returns the original signal', () {
      const size = 256;
      final plan = FftPlan(size);
      final random = math.Random(20261020);
      final real = Float64List(size);
      final imag = Float64List(size);
      for (var i = 0; i < size; i++) {
        real[i] = random.nextDouble() * 2 - 1;
        imag[i] = random.nextDouble() * 2 - 1;
      }
      final originalReal = Float64List.fromList(real);
      final originalImag = Float64List.fromList(imag);
      plan.transform(real, imag);
      plan.inverseTransform(real, imag);
      for (var i = 0; i < size; i++) {
        expect(real[i], closeTo(originalReal[i], 1e-12), reason: 'real[$i]');
        expect(imag[i], closeTo(originalImag[i], 1e-12), reason: 'imag[$i]');
      }
    });

    test('rejects buffers of the wrong length', () {
      final plan = FftPlan(64);
      expect(() => plan.transform(Float64List(32), Float64List(64)),
          throwsArgumentError);
      expect(() => plan.magnitudesOf(Float64List(63)), throwsArgumentError);
    });
  });
}
