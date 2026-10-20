/// A small, honest AnyCodec demo.
///
/// Four tabs, one per thing the Dart layer claims to do:
///
/// 1. **Capabilities** — the report for all twelve codecs in both directions,
///    with every backend, whether it is usable, and why not. The software
///    fallback is highlighted, because "did my encode silently fall back?" is
///    the question the `CodecPath` exists to answer.
/// 2. **Round trip** — a real encode/decode of a synthetic clip through the
///    diagnostic codec, which needs no platform support at all, plus the
///    `CodecPath` the sessions actually reported.
/// 3. **Responsiveness** — a frame counter that keeps ticking while an encode
///    loop runs. Every native call happens on a worker isolate; if that were not
///    true, the counter would stop.
/// 4. **Platform APIs** — the platform-specific extras: the loaded native
///    library and its error channel on native, WebCodecs support on the web.
///
/// Run it with `flutter run` from `example/`. Nothing here is a benchmark: it is
/// a demonstration that the plumbing is wired up.
library;

import 'dart:async';
import 'dart:typed_data';

import 'package:anycodec/anycodec.dart' as anycodec;
import 'package:flutter/material.dart';

void main() {
  runApp(const AnyCodecExampleApp());
}

/// The demo application.
class AnyCodecExampleApp extends StatelessWidget {
  /// Builds the app.
  const AnyCodecExampleApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'AnyCodec',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: Colors.indigo),
        useMaterial3: true,
      ),
      home: const _Home(),
    );
  }
}

class _Home extends StatelessWidget {
  const _Home();

  @override
  Widget build(BuildContext context) {
    return DefaultTabController(
      length: 4,
      child: Scaffold(
        appBar: AppBar(
          title: const Text('AnyCodec'),
          bottom: const TabBar(
            isScrollable: true,
            tabs: <Widget>[
              Tab(text: 'Capabilities'),
              Tab(text: 'Round trip'),
              Tab(text: 'Responsiveness'),
              Tab(text: 'Platform APIs'),
            ],
          ),
        ),
        body: const TabBarView(
          children: <Widget>[
            _CapabilitiesTab(),
            _RoundTripTab(),
            _ResponsivenessTab(),
            _PlatformTab(),
          ],
        ),
      ),
    );
  }
}

// ---------------------------------------------------------------------------
// 1. Capabilities
// ---------------------------------------------------------------------------

class _CapabilitiesTab extends StatefulWidget {
  const _CapabilitiesTab();

  @override
  State<_CapabilitiesTab> createState() => _CapabilitiesTabState();
}

class _CapabilitiesTabState extends State<_CapabilitiesTab> {
  Future<List<anycodec.CodecCapability>>? _capabilities;
  Future<String>? _report;

  @override
  void initState() {
    super.initState();
    _load();
  }

  void _load() {
    setState(() {
      _capabilities = anycodec.AnyCodec.capabilities();
      _report = anycodec.AnyCodec.capabilityReport();
    });
  }

  @override
  Widget build(BuildContext context) {
    return FutureBuilder<List<anycodec.CodecCapability>>(
      future: _capabilities,
      builder: (context, snapshot) {
        if (snapshot.hasError) {
          return _ErrorPane(error: snapshot.error!, onRetry: _load);
        }
        if (!snapshot.hasData) {
          return const Center(child: CircularProgressIndicator());
        }
        final capabilities = snapshot.data!;
        return ListView(
          children: <Widget>[
            Padding(
              padding: const EdgeInsets.all(12),
              child: Row(
                children: <Widget>[
                  Text(
                    '${capabilities.where((entry) => entry.isAvailable).length} '
                    'of ${capabilities.length} codec/direction pairs usable',
                    style: Theme.of(context).textTheme.titleMedium,
                  ),
                  const Spacer(),
                  IconButton(onPressed: _load, icon: const Icon(Icons.refresh)),
                ],
              ),
            ),
            for (final entry in capabilities) _CapabilityCard(entry: entry),
            _RawReport(report: _report),
          ],
        );
      },
    );
  }
}

class _CapabilityCard extends StatelessWidget {
  const _CapabilityCard({required this.entry});

  final anycodec.CodecCapability entry;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Card(
      margin: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: <Widget>[
            Row(
              children: <Widget>[
                Icon(
                  entry.isAvailable ? Icons.check_circle : Icons.cancel,
                  color: entry.isAvailable ? Colors.green : theme.disabledColor,
                  size: 18,
                ),
                const SizedBox(width: 8),
                Text(
                  '${entry.codec.name}  ${entry.direction.token}',
                  style: theme.textTheme.titleMedium,
                ),
                const SizedBox(width: 8),
                if (entry.hardwareAccelerated)
                  const _Badge(text: 'hardware', color: Colors.blue)
                else if (entry.hasSoftwareFallback && entry.isAvailable)
                  const _Badge(text: 'software fallback', color: Colors.orange),
                if (entry.isAvailable &&
                    !entry.hardwareAccelerated &&
                    !entry.hasSoftwareFallback)
                  const _Badge(text: 'native', color: Colors.teal),
              ],
            ),
            const SizedBox(height: 8),
            for (final backend in entry.backends)
              Padding(
                padding: const EdgeInsets.only(left: 26, bottom: 2),
                child: Text(
                  '${backend.backend.token}: '
                  '${backend.available ? 'available' : 'unavailable'}'
                  '${backend.implementation == anycodec.Implementation.software ? ' (software)' : ''}'
                  '${backend.hardware ? ', hardware capable' : ''}'
                  '\n    ${backend.note}',
                  style: theme.textTheme.bodySmall,
                ),
              ),
          ],
        ),
      ),
    );
  }
}

class _RawReport extends StatelessWidget {
  const _RawReport({required this.report});

  final Future<String>? report;

  @override
  Widget build(BuildContext context) {
    return ExpansionTile(
      title: const Text('The report as text'),
      children: <Widget>[
        FutureBuilder<String>(
          future: report,
          builder: (context, snapshot) => Padding(
            padding: const EdgeInsets.all(12),
            child: SelectableText(
              snapshot.data ?? snapshot.error?.toString() ?? '…',
              style: const TextStyle(fontFamily: 'monospace', fontSize: 11),
            ),
          ),
        ),
      ],
    );
  }
}

class _Badge extends StatelessWidget {
  const _Badge({required this.text, required this.color});

  final String text;
  final Color color;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.15),
        borderRadius: BorderRadius.circular(4),
        border: Border.all(color: color),
      ),
      child: Text(text, style: TextStyle(color: color, fontSize: 11)),
    );
  }
}

// ---------------------------------------------------------------------------
// 2. Round trip
// ---------------------------------------------------------------------------

class _RoundTripTab extends StatefulWidget {
  const _RoundTripTab();

  @override
  State<_RoundTripTab> createState() => _RoundTripTabState();
}

class _RoundTripTabState extends State<_RoundTripTab> {
  final List<String> _log = <String>[];
  bool _running = false;
  bool _ok = false;

  Future<void> _run() async {
    setState(() {
      _running = true;
      _ok = false;
      _log.clear();
    });
    final log = <String>[];
    anycodec.AnycodecEncoder? encoder;
    anycodec.AnycodecDecoder? decoder;
    try {
      encoder = await anycodec.AnyCodec.diagnosticEncoder(anycodec.Codec.h264);
      decoder = await anycodec.AnyCodec.diagnosticDecoder(anycodec.Codec.h264);
      log
        ..add('encoder path: ${encoder.path.summary}')
        ..add('worker isolate: ${encoder.workerId}')
        ..add('decoder path: ${decoder.path.summary}')
        ..add('');

      var matched = 0;
      const frames = 5;
      for (var index = 0; index < frames; index++) {
        final pts = index * 33333;
        final frame = _syntheticFrame(pts: pts);
        await encoder.encode(frame);
        final packet = await _firstPacket(encoder);
        if (packet == null) {
          log.add('frame $index: the encoder produced no packet');
          break;
        }
        await decoder.decode(packet);
        final decoded = await _firstFrame(decoder);
        if (decoded is! anycodec.VideoFrame) {
          log.add('frame $index: the decoder produced no picture');
          break;
        }
        final sameGeometry =
            decoded.width == frame.width &&
            decoded.height == frame.height &&
            decoded.pixelFormat == frame.pixelFormat;
        final samePts = decoded.pts == frame.pts;
        final sameBytes = _samePlanes(decoded, frame);
        if (sameGeometry && samePts && sameBytes) {
          matched++;
        }
        log.add(
          'frame $index: ${packet.length} bytes, pts ${frame.pts} -> '
          '${decoded.pts}, geometry ${sameGeometry ? 'ok' : 'DIFFERS'}, '
          'bytes ${sameBytes ? 'identical' : 'DIFFER'}',
        );
      }
      log
        ..add('')
        ..add(
          matched == frames
              ? 'round trip: $matched/$frames frames identical — the whole '
                    'pipeline works on this host'
              : 'round trip: only $matched/$frames frames matched',
        );
      setState(() => _ok = matched == frames);
    } on anycodec.AnyCodecException catch (error) {
      log.add('failed: $error');
    } finally {
      await encoder?.close();
      await decoder?.close();
      if (mounted) {
        setState(() {
          _running = false;
          _log
            ..clear()
            ..addAll(log);
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return ListView(
      padding: const EdgeInsets.all(16),
      children: <Widget>[
        const Text(
          'The diagnostic codec stores a frame verbatim and retrieves it. It is '
          'not registered against any production codec, so it proves the '
          'plumbing — timestamps, buffer ownership, the isolate hand-off, '
          'flush — without needing a media framework on this host.',
        ),
        const SizedBox(height: 12),
        FilledButton.icon(
          onPressed: _running ? null : _run,
          icon: const Icon(Icons.play_arrow),
          label: Text(_running ? 'Running…' : 'Run the round trip'),
        ),
        const SizedBox(height: 12),
        if (_ok)
          const ListTile(
            leading: Icon(Icons.check_circle, color: Colors.green),
            title: Text('Every frame came back byte for byte'),
          ),
        SelectableText(
          _log.isEmpty ? '(nothing yet)' : _log.join('\n'),
          style: const TextStyle(fontFamily: 'monospace', fontSize: 12),
        ),
      ],
    );
  }
}

// ---------------------------------------------------------------------------
// 3. Responsiveness
// ---------------------------------------------------------------------------

class _ResponsivenessTab extends StatefulWidget {
  const _ResponsivenessTab();

  @override
  State<_ResponsivenessTab> createState() => _ResponsivenessTabState();
}

class _ResponsivenessTabState extends State<_ResponsivenessTab> {
  static const int _framesToEncode = 300;

  final Stopwatch _watch = Stopwatch();
  Timer? _timer;
  int _ticks = 0;
  int _lastTickMicros = 0;
  int _longestGapMicros = 0;
  bool _running = false;
  String _conclusion =
      'Press start: a frame counter ticks while 300 frames are '
      'encoded on a worker isolate.';

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  Future<void> _start() async {
    setState(() {
      _running = true;
      _ticks = 0;
      _lastTickMicros = 0;
      _longestGapMicros = 0;
      _conclusion = 'Encoding $_framesToEncode frames…';
    });

    _watch
      ..reset()
      ..start();
    _timer = Timer.periodic(const Duration(milliseconds: 5), (_) {
      final now = _watch.elapsedMicroseconds;
      if (_lastTickMicros != 0) {
        final gap = now - _lastTickMicros;
        if (gap > _longestGapMicros) {
          _longestGapMicros = gap;
        }
      }
      _lastTickMicros = now;
      if (mounted) {
        setState(() => _ticks++);
      }
    });

    anycodec.AnycodecEncoder? encoder;
    try {
      encoder = await anycodec.AnyCodec.diagnosticEncoder(anycodec.Codec.h264);
      for (var index = 0; index < _framesToEncode; index++) {
        await encoder.encode(_syntheticFrame(pts: index * 1000));
        await encoder.receive();
      }
    } finally {
      await encoder?.close();
      _timer?.cancel();
      _watch.stop();
    }

    if (!mounted) {
      return;
    }
    final elapsed = _watch.elapsedMilliseconds;
    setState(() {
      _running = false;
      _conclusion =
          'The counter ticked $_ticks times while $elapsed ms of encoding '
          'happened on another thread.\n'
          'Longest gap between ticks: ${(_longestGapMicros / 1000).toStringAsFixed(1)} ms. '
          '${_longestGapMicros < 250000 ? 'The UI isolate stayed responsive.' : 'The event loop was blocked; that is a bug.'}';
    });
  }

  @override
  Widget build(BuildContext context) {
    return ListView(
      padding: const EdgeInsets.all(16),
      children: <Widget>[
        const Text(
          'Every native call in this package runs on a worker isolate. If that '
          'were not true, this counter would freeze for the whole encode loop.',
        ),
        const SizedBox(height: 16),
        Center(
          child: Column(
            children: <Widget>[
              Text('$_ticks', style: Theme.of(context).textTheme.displayMedium),
              const Text('timer ticks (every 5 ms)'),
            ],
          ),
        ),
        const SizedBox(height: 16),
        FilledButton.icon(
          onPressed: _running ? null : _start,
          icon: const Icon(Icons.timer),
          label: Text(_running ? 'Encoding…' : 'Start the encode loop'),
        ),
        const SizedBox(height: 16),
        Text(_conclusion),
      ],
    );
  }
}

// ---------------------------------------------------------------------------
// 4. Platform APIs
// ---------------------------------------------------------------------------

class _PlatformTab extends StatefulWidget {
  const _PlatformTab();

  @override
  State<_PlatformTab> createState() => _PlatformTabState();
}

class _PlatformTabState extends State<_PlatformTab> {
  Future<String>? _report;

  @override
  void initState() {
    super.initState();
    _report = anycodec.AnyCodec.platformReport();
  }

  void _reload() {
    setState(() => _report = anycodec.AnyCodec.platformReport());
  }

  @override
  Widget build(BuildContext context) {
    return FutureBuilder<String>(
      future: _report,
      builder: (context, snapshot) {
        if (snapshot.hasError) {
          return _ErrorPane(error: snapshot.error!, onRetry: _reload);
        }
        if (!snapshot.hasData) {
          return const Center(child: CircularProgressIndicator());
        }
        return ListView(
          padding: const EdgeInsets.all(16),
          children: <Widget>[
            const Text(
              'This report comes from the platform-specific APIs in '
              'package:anycodec/anycodec_platform.dart: NativeLibraryInfo and '
              'LastNativeError on native, WebCodecs on the web.',
            ),
            const SizedBox(height: 8),
            const Text(
              'That library is conditional — its native extras only exist where '
              'dart:io does, and its web extras only where dart:js_interop does — '
              'so UI code that must compile for every platform asks AnyCodec for '
              'this text rendering instead of naming a platform-only class.',
            ),
            const SizedBox(height: 12),
            Align(
              alignment: Alignment.centerLeft,
              child: OutlinedButton.icon(
                onPressed: _reload,
                icon: const Icon(Icons.refresh),
                label: const Text('Refresh'),
              ),
            ),
            const SizedBox(height: 12),
            SelectableText(
              snapshot.data!,
              style: const TextStyle(fontFamily: 'monospace', fontSize: 12),
            ),
          ],
        );
      },
    );
  }
}

// ---------------------------------------------------------------------------
// Shared bits
// ---------------------------------------------------------------------------

class _ErrorPane extends StatelessWidget {
  const _ErrorPane({required this.error, required this.onRetry});

  final Object error;
  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: <Widget>[
            const Icon(Icons.error_outline, size: 40),
            const SizedBox(height: 12),
            Text('$error', textAlign: TextAlign.center),
            const SizedBox(height: 12),
            OutlinedButton(onPressed: onRetry, child: const Text('Retry')),
          ],
        ),
      ),
    );
  }
}

/// A 64x64 I420 picture whose planes vary with [pts], so a mismatch is visible.
anycodec.VideoFrame _syntheticFrame({
  int pts = 0,
  int width = 64,
  int height = 64,
}) {
  final y = Uint8List(width * height);
  for (var index = 0; index < y.length; index++) {
    y[index] = (index + pts) & 0xff;
  }
  final chroma = (width ~/ 2) * (height ~/ 2);
  final u = Uint8List(chroma);
  final v = Uint8List(chroma);
  for (var index = 0; index < chroma; index++) {
    u[index] = (0x80 + index) & 0xff;
    v[index] = (0x40 + index + pts) & 0xff;
  }
  return anycodec.VideoFrame.i420(
    width: width,
    height: height,
    y: y,
    u: u,
    v: v,
    pts: pts,
    duration: 33333,
    keyframe: true,
  );
}

bool _samePlanes(anycodec.VideoFrame left, anycodec.VideoFrame right) {
  if (left.planes.length != right.planes.length) {
    return false;
  }
  for (var index = 0; index < left.planes.length; index++) {
    final a = left.planes[index];
    final b = right.planes[index];
    if (a.length != b.length) {
      return false;
    }
    for (var offset = 0; offset < a.length; offset++) {
      if (a[offset] != b[offset]) {
        return false;
      }
    }
  }
  return true;
}

Future<anycodec.Packet?> _firstPacket(anycodec.AnycodecEncoder encoder) async {
  for (var attempt = 0; attempt < 4; attempt++) {
    final packet = await encoder.receive();
    if (packet != null) {
      return packet;
    }
  }
  return null;
}

Future<anycodec.Frame?> _firstFrame(anycodec.AnycodecDecoder decoder) async {
  for (var attempt = 0; attempt < 4; attempt++) {
    final frame = await decoder.receive();
    if (frame != null) {
      return frame;
    }
  }
  return null;
}
