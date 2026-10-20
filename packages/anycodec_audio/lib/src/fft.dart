import 'dart:math' as math;
import 'dart:typed_data';

/// A reusable in-place radix-2 FFT plan.
///
/// The tables (bit-reversal permutation and twiddles) are computed once in the
/// constructor, so repeated transforms of the same size do no trigonometry at
/// all. That is the whole difference between an FFT you can run per video frame
/// and one you cannot: a naive implementation recomputing `cos`/`sin` per butterfly
/// spends most of its time in the math library.
///
/// Only power-of-two sizes are supported. That is not a limitation worth working
/// around here: a spectrum visualiser chooses its own size, and the fast
/// power-of-two path is what makes real-time analysis possible.
///
/// Everything is `float64` deliberately. `float32` would halve the memory traffic
/// and lose about three decimal digits of dynamic range, which shows up as a
/// noisy floor in a dB spectrum — the one place this data gets displayed.
final class FftPlan {
  /// Builds a plan for [size] points.
  ///
  /// Throws [ArgumentError] when [size] is not a power of two or is zero.
  FftPlan(this.size)
    : _cos = Float64List(size ~/ 2),
      _sin = Float64List(size ~/ 2),
      _reversed = Uint32List(size) {
    if (size < 2 || (size & (size - 1)) != 0) {
      throw ArgumentError.value(
        size,
        'size',
        'must be a power of two greater than one (a radix-2 plan)',
      );
    }
    final half = size ~/ 2;
    for (var i = 0; i < half; i++) {
      final angle = -2.0 * math.pi * i / size;
      _cos[i] = math.cos(angle);
      _sin[i] = math.sin(angle);
    }
    final bits = size.bitLength - 1;
    for (var i = 0; i < size; i++) {
      var value = i;
      var reversed = 0;
      for (var bit = 0; bit < bits; bit++) {
        reversed = (reversed << 1) | (value & 1);
        value >>= 1;
      }
      _reversed[i] = reversed;
    }
  }

  /// Number of points.
  final int size;

  /// Number of non-redundant bins for a real input, `size / 2 + 1`.
  int get binCount => size ~/ 2 + 1;

  /// Frequency of a bin for [sampleRate].
  double binFrequency(int bin, int sampleRate) =>
      bin * sampleRate / size;

  final Float64List _cos;
  final Float64List _sin;
  final Uint32List _reversed;

  /// Forward transform, in place.
  ///
  /// [real] and [imag] must both be exactly [size] long. For a real signal,
  /// [imag] is zero on entry.
  void transform(Float64List real, Float64List imag) {
    _checkLengths(real, imag);
    _permute(real, imag);
    _butterflies(real, imag);
  }

  /// Inverse transform, in place, including the `1/size` scaling.
  void inverseTransform(Float64List real, Float64List imag) {
    _checkLengths(real, imag);
    _permute(real, imag);
    // The inverse kernel is the forward one with conjugated twiddles.
    _butterflies(real, imag, conjugate: true);
    final scale = 1.0 / size;
    for (var i = 0; i < size; i++) {
      real[i] *= scale;
      imag[i] *= scale;
    }
  }

  /// Magnitudes of the non-redundant bins of a real input.
  ///
  /// [real] is not modified. The returned list has [binCount] entries; bin 0 is
  /// DC and the last bin is the Nyquist frequency.
  Float64List magnitudesOf(Float64List real) {
    if (real.length != size) {
      throw ArgumentError.value(real.length, 'real', 'expected $size samples');
    }
    final re = Float64List.fromList(real);
    final im = Float64List(size);
    transform(re, im);
    final out = Float64List(binCount);
    // One-sided spectrum normalisation. Bins `1 .. binCount-2` each carry half the
    // energy of a real sinusoid, so they are doubled; bin 0 (DC) and the Nyquist
    // bin are not mirrored and must not be. Doubling DC as well is a common
    // shortcut and makes a constant signal read +6 dB.
    final mirrored = 2.0 / size;
    final single = 1.0 / size;
    for (var i = 0; i < binCount; i++) {
      final magnitude = math.sqrt(re[i] * re[i] + im[i] * im[i]);
      out[i] = magnitude * (i == 0 || i == binCount - 1 ? single : mirrored);
    }
    return out;
  }

  void _checkLengths(Float64List real, Float64List imag) {
    if (real.length != size || imag.length != size) {
      throw ArgumentError(
        'expected two Float64List of length $size, got '
        '${real.length} and ${imag.length}',
      );
    }
  }

  void _permute(Float64List real, Float64List imag) {
    for (var i = 0; i < size; i++) {
      final j = _reversed[i];
      if (j > i) {
        final tr = real[i];
        real[i] = real[j];
        real[j] = tr;
        final ti = imag[i];
        imag[i] = imag[j];
        imag[j] = ti;
      }
    }
  }

  void _butterflies(
    Float64List real,
    Float64List imag, {
    bool conjugate = false,
  }) {
    final sign = conjugate ? -1.0 : 1.0;
    for (var span = 2; span <= size; span <<= 1) {
      final halfSpan = span >> 1;
      final stride = size ~/ span;
      for (var start = 0; start < size; start += span) {
        var twiddle = 0;
        for (var offset = 0; offset < halfSpan; offset++) {
          final cos = _cos[twiddle];
          final sin = sign * _sin[twiddle];
          final a = start + offset;
          final b = a + halfSpan;
          final tRe = real[b] * cos - imag[b] * sin;
          final tIm = real[b] * sin + imag[b] * cos;
          real[b] = real[a] - tRe;
          imag[b] = imag[a] - tIm;
          real[a] += tRe;
          imag[a] += tIm;
          twiddle += stride;
        }
      }
    }
  }
}
