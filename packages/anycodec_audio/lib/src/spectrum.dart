import 'dart:math' as math;
import 'dart:typed_data';

import 'fft.dart';
import 'pcm_format.dart';

/// The taper applied before the FFT.
///
/// A rectangular window (no taper) is the most faithful and the worst to look at:
/// a tone that does not land exactly on a bin leaks into its neighbours as
/// sidelobes, which on screen is a smear around every peak. Hann is the usual
/// default for a display; Blackman has the lowest sidelobes and the widest main
/// lobe.
enum SpectrumWindow {
  /// No taper. Highest frequency resolution, worst leakage.
  rectangular,

  /// Raised cosine. The default: a good compromise for a spectrum display.
  hann,

  /// Slightly narrower main lobe than Hann, but sidelobes do not decay as fast.
  hamming,

  /// Three-term cosine. Lowest sidelobes; peaks look cleaner, slightly wider.
  blackman;

  /// The taper coefficient for sample [index] of [size].
  double coefficientAt(int index, int size) {
    if (size <= 1) {
      return 1.0;
    }
    final n = index / (size - 1);
    return switch (this) {
      SpectrumWindow.rectangular => 1.0,
      SpectrumWindow.hann => 0.5 - 0.5 * math.cos(2 * math.pi * n),
      SpectrumWindow.hamming => 0.54 - 0.46 * math.cos(2 * math.pi * n),
      SpectrumWindow.blackman =>
        0.42 - 0.5 * math.cos(2 * math.pi * n) + 0.08 * math.cos(4 * math.pi * n),
    };
  }
}

/// One analysed spectrum, ready to draw.
///
/// Both representations are provided because the two kinds of drawing want
/// different things: a detailed analyser wants [magnitudesDb] per bin, and a bar
/// meter wants [bandValues], which is already in the `0..1` range a painter needs
/// and already logarithmically spaced so that music does not pile up in the left
/// tenth of the screen.
final class SpectrumFrame {
  /// Creates a frame. Not usually called directly: [SpectrumAnalyzer] builds it.
  SpectrumFrame({
    required this.magnitudes,
    required this.magnitudesDb,
    required this.bandValues,
    required this.sampleRate,
    required this.fftSize,
    required this.bandEdgesHz,
  });

  /// Linear magnitude per non-redundant FFT bin, where a full-scale sine is 1.0.
  final Float64List magnitudes;

  /// The same in dBFS, floored at [floorDb]. This is what a dB-scaled display
  /// draws.
  final Float64List magnitudesDb;

  /// Log-spaced band levels in `0..1`, one per band, already normalised.
  final Float64List bandValues;

  /// Sample rate of the analysed audio.
  final int sampleRate;

  /// FFT size that produced this frame.
  final int fftSize;

  /// The band edges in Hz, `bandValues.length + 1` entries, for labelling an axis.
  final Float64List bandEdgesHz;

  /// The floor applied when converting to dB.
  static const double floorDb = -100.0;

  /// Number of non-redundant bins.
  int get binCount => magnitudes.length;

  /// Frequency of bin [bin], in Hz.
  double binFrequency(int bin) => bin * sampleRate / fftSize;

  /// Index of the loudest bin, ignoring DC.
  int get peakBin {
    var best = 0;
    var bestValue = -1.0;
    for (var i = 1; i < magnitudes.length; i++) {
      if (magnitudes[i] > bestValue) {
        bestValue = magnitudes[i];
        best = i;
      }
    }
    return best;
  }

  /// Frequency of the loudest bin, in Hz.
  double get peakFrequency => binFrequency(peakBin);

  @override
  String toString() =>
      'SpectrumFrame($fftSize-point, ${bandValues.length} bands, '
      'peak ${peakFrequency.toStringAsFixed(1)}Hz)';
}

/// Turns a stream of PCM into [SpectrumFrame]s.
///
/// This is the "data for drawing" half of the package, and it stops there: it
/// renders nothing, owns no timer and knows nothing about a player. A display
/// calls [add] as audio arrives and [analyze] once per frame.
///
/// The analyser keeps a sliding window of the most recent [fftSize] mono samples.
/// Feeding more than [fftSize] samples between analyses is fine — only the tail is
/// used, which is what you want for a real-time display.
final class SpectrumAnalyzer {
  /// Builds an analyser.
  ///
  /// [fftSize] must be a power of two (see `FftPlan`). [bands] is how many
  /// log-spaced bars to produce, and `minFrequency`/`maxFrequency` bound them.
  SpectrumAnalyzer({
    required this.fftSize,
    required this.sampleRate,
    this.window = SpectrumWindow.hann,
    int bands = 64,
    double minFrequency = 20.0,
    double maxFrequency = 20000.0,
    this.smoothing = 0.0,
  }) : _fft = FftPlan(fftSize),
       _windowed = Float64List(fftSize),
       _history = Float64List(fftSize),
       _bandEdges = Float64List(bands + 1),
       _bands = bands,
       _minHz = minFrequency,
       _maxHz = maxFrequency,
       _previous = Float64List(bands) {
    if (bands <= 0) {
      throw ArgumentError.value(bands, 'bands', 'must be positive');
    }
    if (!(minFrequency > 0 && maxFrequency > minFrequency)) {
      throw ArgumentError(
        'need 0 < minFrequency < maxFrequency, got '
        '$minFrequency..$maxFrequency',
      );
    }
    final nyquist = sampleRate / 2.0;
    final top = maxFrequency < nyquist ? maxFrequency : nyquist;
    // Log spacing: equal ratios per band, which matches how hearing works and
    // spreads the useful spectrum across the display evenly.
    final ratio = top / minFrequency;
    for (var band = 0; band <= bands; band++) {
      _bandEdges[band] = minFrequency * math.pow(ratio, band / bands);
    }
    // Precompute the taper once; it never changes. (It must be allocated once:
    // `_taper` is `late final`, so assigning it per iteration is a run-time error
    // for the second sample.)
    _taper = Float64List(fftSize);
    var gain = 0.0;
    for (var i = 0; i < fftSize; i++) {
      _taper[i] = window.coefficientAt(i, fftSize);
      gain += _taper[i];
    }
    // Coherent gain: a tapered full-scale sine reads *below* full scale by the
    // window's mean, which for Hann is 0.5 — a constant -6 dB error that would
    // make every display built on this look quiet. Dividing it out makes 0 dBFS
    // mean what it says, for every window.
    _coherentGain = gain / fftSize;
  }

  /// Number of points per transform.
  final int fftSize;

  /// Sample rate of the audio being fed in.
  final int sampleRate;

  /// Taper applied before the transform.
  final SpectrumWindow window;

  /// Exponential smoothing applied to band values, `0..1`.
  ///
  /// `0` is raw (jumpy, honest); higher values make a bar meter settle instead of
  /// flickering. This is a display concern, so the default is `0`: the package
  /// does not quietly soften data.
  final double smoothing;

  final FftPlan _fft;
  final Float64List _windowed;
  final Float64List _history;
  final Float64List _bandEdges;
  final Float64List _previous;
  final int _bands;
  final double _minHz;
  final double _maxHz;
  late final Float64List _taper;
  late final double _coherentGain;
  int _write = 0;
  int _filled = 0;

  /// How many of the last [fftSize] samples are real (not leading silence).
  int get filled => _filled;

  /// Band edges in Hz, `bands + 1` entries.
  Float64List get bandEdgesHz => Float64List.fromList(_bandEdges);

  /// Discards the sliding window and the smoothing state.
  ///
  /// The sample history is zeroed as well, not merely the indices: leaving it in
  /// place makes the next [analyze] report the audio from before the reset, which
  /// looks exactly like a broken visualiser.
  void reset() {
    _write = 0;
    _filled = 0;
    _history.fillRange(0, _history.length, 0);
    _previous.fillRange(0, _previous.length, 0);
  }

  /// Feeds interleaved [samples] with [channels] channels.
  ///
  /// Multichannel input is averaged to mono first: a spectrum of one channel of a
  /// stereo mix is a spectrum of half the music.
  void add(Float32List samples, {int channels = 1}) {
    final mono = channels == 1 ? samples : mixToMono(samples, channels);
    for (var i = 0; i < mono.length; i++) {
      _history[_write] = mono[i];
      _write = (_write + 1) % fftSize;
    }
    _filled = math.min(fftSize, _filled + mono.length);
  }

  /// Analyses the window and returns a frame.
  ///
  /// Leading silence (before [fftSize] samples have been fed) is zeros, which is
  /// the honest thing to draw before there is data. Repeated calls without new
  /// input return the same analysis.
  SpectrumFrame analyze() {
    // Linearise the ring, oldest first, into the windowed buffer.
    final start = (_write - fftSize) % fftSize;
    for (var i = 0; i < fftSize; i++) {
      final index = (start + i) % fftSize;
      _windowed[i] = _history[index] * _taper[i];
    }
    final magnitudes = _fft.magnitudesOf(_windowed);
    // Undo the window's coherent gain so 0 dBFS means 0 dBFS.
    for (var i = 0; i < magnitudes.length; i++) {
      magnitudes[i] /= _coherentGain;
    }
    final db = Float64List(magnitudes.length);
    for (var i = 0; i < magnitudes.length; i++) {
      final value = magnitudes[i];
      final d = value <= 0 ? -200.0 : 20.0 * math.log(value) / math.ln10;
      db[i] = d < SpectrumFrame.floorDb ? SpectrumFrame.floorDb : d;
    }
    final bandValues = _bandValues(magnitudes);
    return SpectrumFrame(
      magnitudes: magnitudes,
      magnitudesDb: db,
      bandValues: bandValues,
      sampleRate: sampleRate,
      fftSize: fftSize,
      bandEdgesHz: Float64List.fromList(_bandEdges),
    );
  }

  Float64List _bandValues(Float64List magnitudes) {
    final out = Float64List(_bands);
    final binHz = sampleRate / fftSize;
    for (var band = 0; band < _bands; band++) {
      final lowHz = _bandEdges[band];
      final highHz = _bandEdges[band + 1];
      var lowBin = (lowHz / binHz).floor();
      var highBin = (highHz / binHz).ceil();
      if (lowBin < 0) {
        lowBin = 0;
      }
      if (highBin > magnitudes.length - 1) {
        highBin = magnitudes.length - 1;
      }
      // Take the peak inside the band: averaging would bury a tone under the
      // quiet bins next to it and a bar meter would never reach the top.
      var peak = 0.0;
      for (var bin = lowBin; bin <= highBin; bin++) {
        if (magnitudes[bin] > peak) {
          peak = magnitudes[bin];
        }
      }
      final d = peak <= 0 ? -200.0 : 20.0 * math.log(peak) / math.ln10;
      var normalised = (d - SpectrumFrame.floorDb) /
          (0.0 - SpectrumFrame.floorDb);
      if (normalised < 0) {
        normalised = 0;
      } else if (normalised > 1) {
        normalised = 1;
      }
      if (smoothing > 0) {
        normalised = _previous[band] * smoothing + normalised * (1 - smoothing);
        _previous[band] = normalised;
      }
      out[band] = normalised;
    }
    return out;
  }

  /// Lower edge of band 0, in Hz.
  double get minFrequency => _minHz;

  /// Upper edge of the last band, in Hz.
  double get maxFrequency => _maxHz;
}
