import 'dart:async';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:anycodec_audio/anycodec_audio.dart';
import 'package:anycodec_video/anycodec_video.dart';
import 'package:flutter/material.dart';

/// A demonstration player built **only** on `anycodec_video` and
/// `anycodec_audio`.
///
/// It is deliberately small, and it exists to answer two questions with numbers
/// rather than adjectives:
///
/// * can frames be put on screen at a steady rate, and what does one cost? (the
///   Video tab drives `AnycodecVideoSurface.present`);
/// * can an audio stream be pulled by a device-shaped loop while a spectrum is
///   computed from the same samples, and what does that cost? (the Audio tab
///   drives `PcmStreamSource` + `SpectrumAnalyzer`).
///
/// What it is *not*: a container reader, a seeker, or an audible player. There is
/// no demuxer in these packages yet, and the audio output backend is still an
/// open decision — so the Audio tab runs the exact loop a player would run and
/// says so instead of pretending to make sound.
void main() => runApp(const PlayerApp());

/// The demo application.
class PlayerApp extends StatelessWidget {
  /// Creates the app.
  const PlayerApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'AnyCodec player demo',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(colorSchemeSeed: const Color(0xFF3B82F6)),
      home: const _Home(),
    );
  }
}

class _Home extends StatelessWidget {
  const _Home();

  @override
  Widget build(BuildContext context) {
    return DefaultTabController(
      length: 2,
      child: Scaffold(
        appBar: AppBar(
          title: const Text('AnyCodec player demo'),
          bottom: const TabBar(
            tabs: [
              Tab(icon: Icon(Icons.movie_outlined), text: 'Video'),
              Tab(icon: Icon(Icons.graphic_eq), text: 'Audio'),
            ],
          ),
        ),
        body: const TabBarView(children: [_VideoDemo(), _AudioDemo()]),
      ),
    );
  }
}

/// Rolling timing statistics for one demo.
class _Stats {
  final List<double> _samples = <double>[];
  int frames = 0;
  final Stopwatch _since = Stopwatch()..start();

  void add(double milliseconds) {
    frames++;
    _samples.add(milliseconds);
    if (_samples.length > 120) {
      _samples.removeAt(0);
    }
  }

  double get averageMs => _samples.isEmpty
      ? 0
      : _samples.reduce((a, b) => a + b) / _samples.length;

  double get maxMs => _samples.isEmpty ? 0 : _samples.reduce(math.max);

  double get fps {
    final elapsed = _since.elapsedMilliseconds;
    return elapsed == 0 ? 0 : frames * 1000 / elapsed;
  }
}

// ---------------------------------------------------------------------------
// Video
// ---------------------------------------------------------------------------

/// Presents a generated frame on a ~60 Hz clock and reports the per-frame cost
/// plus which presentation backend was actually chosen.
class _VideoDemo extends StatefulWidget {
  const _VideoDemo();

  @override
  State<_VideoDemo> createState() => _VideoDemoState();
}

class _VideoDemoState extends State<_VideoDemo> {
  static const int _width = 320;
  static const int _height = 180;

  AnycodecVideoSurface? _surface;
  String _error = '';
  final _Stats _stats = _Stats();
  final Float32List _pixels = Float32List(_width * _height * 4);
  Timer? _timer;
  int _frame = 0;
  bool _busy = false;

  @override
  void initState() {
    super.initState();
    _start();
  }

  Future<void> _start() async {
    try {
      final surface = await AnycodecVideo.create(
        width: _width,
        height: _height,
        format: VideoPixelFormat.rgba8888,
      );
      if (!mounted) {
        await surface.dispose();
        return;
      }
      setState(() => _surface = surface);
      _timer = Timer.periodic(const Duration(milliseconds: 16), (_) {
        _tick();
      });
    } catch (error) {
      if (mounted) {
        setState(() => _error = '$error');
      }
    }
  }

  Future<void> _tick() async {
    final surface = _surface;
    // Skip rather than queue: a loop that cannot keep up must drop frames, not
    // build an ever-growing backlog.
    if (surface == null || _busy) {
      return;
    }
    _busy = true;
    final watch = Stopwatch()..start();
    try {
      _paint(_frame++);
      await surface.present(_bytes);
      watch.stop();
      _stats.add(watch.elapsedMicroseconds / 1000);
      if (mounted && _frame % 6 == 0) {
        setState(() {});
      }
    } catch (error) {
      _timer?.cancel();
      if (mounted) {
        setState(() => _error = '$error');
      }
    } finally {
      _busy = false;
    }
  }

  Uint8List get _bytes => _pixels.buffer.asUint8List();

  /// A moving test pattern: diagonal bars whose phase advances per frame, plus a
  /// brightness ramp so a torn or sheared frame is obvious rather than subtle.
  void _paint(int frame) {
    final data = _bytes;
    final phase = (frame % 120) * 2;
    var i = 0;
    for (var y = 0; y < _height; y++) {
      final rowShade = 40 + (y * 120 ~/ _height);
      for (var x = 0; x < _width; x++) {
        final band = ((x + y + phase) ~/ 12) % 2;
        data[i++] = band == 0 ? 240 : rowShade; // R
        data[i++] = band == 0 ? 120 : rowShade; // G
        data[i++] = band == 0 ? 40 : rowShade; // B
        data[i++] = 255; // A
      }
    }
    // A white marker sweeping horizontally makes a dropped frame visible.
    final markerX = (frame * 3) % _width;
    for (var y = 0; y < _height; y++) {
      final base = (y * _width + markerX) * 4;
      data[base] = 255;
      data[base + 1] = 255;
      data[base + 2] = 255;
      data[base + 3] = 255;
    }
  }

  @override
  void dispose() {
    _timer?.cancel();
    _surface?.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final surface = _surface;
    return Column(
      children: [
        Expanded(
          child: Container(
            color: Colors.black,
            alignment: Alignment.center,
            child: _error.isNotEmpty
                ? Padding(
                    padding: const EdgeInsets.all(24),
                    child: Text(
                      _error,
                      style: const TextStyle(color: Colors.redAccent),
                      textAlign: TextAlign.center,
                    ),
                  )
                : surface == null
                ? const CircularProgressIndicator()
                : surface.build(),
          ),
        ),
        _StatsBar(
          lines: [
            if (surface != null) surface.path.summary,
            'frames: ${_stats.frames}   present: '
                '${_stats.averageMs.toStringAsFixed(2)} ms avg / '
                '${_stats.maxMs.toStringAsFixed(2)} ms max',
            'loop rate: ${_stats.fps.toStringAsFixed(1)} fps  '
                '($_width x $_height, ${_width * _height * 4 ~/ 1024} KiB/frame)',
          ],
        ),
      ],
    );
  }
}

// ---------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------

/// Runs the two loops a player owns — a producer pushing decoded PCM and a
/// consumer pulling device-sized blocks — and draws the spectrum of the result.
class _AudioDemo extends StatefulWidget {
  const _AudioDemo();

  @override
  State<_AudioDemo> createState() => _AudioDemoState();
}

class _AudioDemoState extends State<_AudioDemo> {
  static const int _sampleRate = 48000;
  static const int _channels = 2;
  static const int _producerFrames = 960; // 20 ms at 48 kHz
  static const int _deviceFrames = 480; // 10 ms at 48 kHz

  late final PcmFormat _format;
  late final PcmStreamSource _source;
  late final SpectrumAnalyzer _analyzer;
  final _Stats _producerStats = _Stats();
  final _Stats _consumerStats = _Stats();
  Timer? _producer;
  Timer? _consumer;
  SpectrumFrame? _spectrum;
  int _startFrame = 0;
  int _pulledFrames = 0;

  @override
  void initState() {
    super.initState();
    _format = PcmFormat(
      sampleFormat: PcmSampleFormat.f32,
      sampleRate: _sampleRate,
      channels: _channels,
    );
    _source = PcmStreamSource(format: _format, capacityFrames: _sampleRate);
    _analyzer = SpectrumAnalyzer(
      fftSize: 2048,
      sampleRate: _sampleRate,
      bands: 48,
    );
    // Producer: what a decoder would deliver, every 20 ms.
    _producer = Timer.periodic(const Duration(milliseconds: 20), (_) {
      final watch = Stopwatch()..start();
      _source.addFloat32(_synthesise());
      watch.stop();
      _producerStats.add(watch.elapsedMicroseconds / 1000);
    });
    // Consumer: what a device callback would pull, every 10 ms.
    _consumer = Timer.periodic(const Duration(milliseconds: 10), (_) {
      final watch = Stopwatch()..start();
      var blocks = 0;
      while (blocks < 4) {
        final chunk = Float32List(_deviceFrames * _channels);
        final frames = _source.read(chunk);
        if (frames == 0) {
          break;
        }
        _analyzer.add(chunk, channels: _channels);
        _pulledFrames += frames;
        blocks++;
      }
      _spectrum = _analyzer.analyze();
      watch.stop();
      _consumerStats.add(watch.elapsedMicroseconds / 1000);
      if (mounted && blocks > 0) {
        setState(() {});
      }
    });
  }

  /// A steady 440 Hz reference plus a gliding tone, so the display has something
  /// to track.
  Float32List _synthesise() {
    final out = Float32List(_producerFrames * _channels);
    for (var frame = 0; frame < _producerFrames; frame++) {
      final t = (_startFrame + frame) / _sampleRate;
      final glide = 200 + 600 * (0.5 + 0.5 * math.sin(2 * math.pi * 0.2 * t));
      final sample =
          0.25 * math.sin(2 * math.pi * 440 * t) +
          0.25 * math.sin(2 * math.pi * glide * t);
      for (var channel = 0; channel < _channels; channel++) {
        out[frame * _channels + channel] = sample;
      }
    }
    _startFrame += _producerFrames;
    return out;
  }

  @override
  void dispose() {
    _producer?.cancel();
    _consumer?.cancel();
    _source.close();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final spectrum = _spectrum;
    return Column(
      children: [
        Expanded(
          child: Container(
            color: const Color(0xFF0B1220),
            child: spectrum == null
                ? const Center(child: CircularProgressIndicator())
                : CustomPaint(
                    painter: _SpectrumPainter(spectrum.bandValues),
                    child: const SizedBox.expand(),
                  ),
          ),
        ),
        _StatsBar(
          lines: [
            'pcm stream: ${_format.sampleRate} Hz x${_format.channels} '
                '${_format.sampleFormat.name}, '
                '${(_pulledFrames / _sampleRate).toStringAsFixed(2)} s pulled',
            'producer: ${_producerStats.averageMs.toStringAsFixed(3)} ms per '
                '20 ms block   consumer: '
                '${_consumerStats.averageMs.toStringAsFixed(3)} ms at '
                '${_consumerStats.fps.toStringAsFixed(0)} Hz',
            if (spectrum != null)
              'spectrum: ${spectrum.fftSize}-point, peak '
                  '${spectrum.peakFrequency.toStringAsFixed(0)} Hz, '
                  '${spectrum.magnitudesDb[spectrum.peakBin].toStringAsFixed(1)} dBFS',
            'no device is opened here: choosing an audio output backend is a '
                'separate decision, so this runs the loop and draws it',
          ],
        ),
      ],
    );
  }
}

/// Draws band levels as bars. This is the "drawing data" half — the package hands
/// over numbers, the application decides what a bar looks like.
class _SpectrumPainter extends CustomPainter {
  _SpectrumPainter(this.bands);

  final Float64List bands;

  @override
  void paint(Canvas canvas, Size size) {
    if (bands.isEmpty) {
      return;
    }
    final slot = size.width / bands.length;
    final barWidth = math.max(1.0, slot * 0.72);
    final paint = Paint()..style = PaintingStyle.fill;
    for (var i = 0; i < bands.length; i++) {
      final level = bands[i].clamp(0.0, 1.0);
      final height = level * size.height;
      // Blue at the bottom, warm at the top: reads as loudness without a legend.
      paint.color = Color.lerp(
        const Color(0xFF2563EB),
        const Color(0xFFF59E0B),
        level,
      )!;
      final left = i * slot + (slot - barWidth) / 2;
      canvas.drawRRect(
        RRect.fromRectAndRadius(
          Rect.fromLTWH(left, size.height - height, barWidth, height),
          const Radius.circular(2),
        ),
        paint,
      );
    }
  }

  @override
  bool shouldRepaint(_SpectrumPainter oldDelegate) => true;
}

// ---------------------------------------------------------------------------
// Shared chrome
// ---------------------------------------------------------------------------

class _StatsBar extends StatelessWidget {
  const _StatsBar({required this.lines});

  final List<String> lines;

  @override
  Widget build(BuildContext context) {
    final style = Theme.of(context).textTheme.bodySmall?.copyWith(
      fontFamily: 'monospace',
      color: Colors.white70,
    );
    return Container(
      width: double.infinity,
      color: const Color(0xFF111827),
      padding: const EdgeInsets.fromLTRB(12, 10, 12, 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          for (final line in lines)
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 2),
              child: Text(line, style: style),
            ),
        ],
      ),
    );
  }
}
