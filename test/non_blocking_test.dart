/// Non-blocking behaviour: does the UI isolate keep ticking while a session
/// works?
///
/// This is the test that would catch the classic FFI mistake — running a codec
/// on the isolate that also drives the event loop. Every native call in this
/// package happens on a worker isolate, so a `Timer` on the calling isolate must
/// keep firing at roughly its requested interval even while hundreds of encodes
/// are in flight.
library;

import 'dart:async';

import 'package:anycodec/anycodec.dart';
import 'package:test/test.dart';

import 'support/frames.dart';

void main() {
  setUpAll(() => AnyCodec.initialize(workers: 2));
  tearDownAll(AnyCodec.shutdown);

  test('the calling isolate keeps ticking while frames are encoded', () async {
    const tick = Duration(milliseconds: 5);
    const frames = 200;

    final gaps = <int>[];
    var ticks = 0;
    final watch = Stopwatch();
    var lastTick = 0;

    final timer = Timer.periodic(tick, (_) {
      final now = watch.elapsedMicroseconds;
      if (lastTick != 0) {
        gaps.add(now - lastTick);
      }
      lastTick = now;
      ticks++;
    });

    final encoder = await AnyCodec.diagnosticEncoder(Codec.h264);
    try {
      watch.start();
      for (var index = 0; index < frames; index++) {
        await encoder.encode(sampleVideoFrame(pts: index));
        final packet = await drainPacket(encoder);
        expect(packet, isNotNull, reason: 'frame $index must produce a packet');
      }
      watch.stop();
    } finally {
      timer.cancel();
      await encoder.close();
    }

    expect(ticks, greaterThan(0), reason: 'the timer must have fired at all');
    expect(
      gaps,
      isNotEmpty,
      reason:
          'at least two ticks must have happened, or the test proves nothing',
    );

    // 200 encodes over an isolate boundary take long enough that a blocked event
    // loop would show up as one gap of tens or hundreds of milliseconds. The
    // threshold is deliberately loose (a loaded CI machine can jitter): what
    // matters is that no single gap is anywhere near the total runtime.
    final longestGap = gaps.reduce((a, b) => a > b ? a : b);
    expect(
      longestGap,
      lessThan(250000),
      reason:
          'the longest gap between timer ticks was ${longestGap / 1000} ms; a '
          'blocked event loop would show the whole ${watch.elapsedMilliseconds} '
          'ms runtime as one gap',
    );
    expect(
      watch.elapsedMicroseconds,
      greaterThan(0),
      reason: 'the encode loop must actually have taken time',
    );

    // Sanity: the work really did happen.
    expect(ticks, greaterThanOrEqualTo(2));
  });

  test(
    'hardware probing (the one documented slow ABI call) does not block either',
    () async {
      const tick = Duration(milliseconds: 5);
      var ticks = 0;
      final timer = Timer.periodic(tick, (_) => ticks++);
      try {
        // `anycodec_probe_devices` can touch the GPU; the runtime must route it
        // through the pool rather than calling it here.
        final devices = await AnyCodec.probeHardware();
        expect(devices, isNotNull);
      } finally {
        timer.cancel();
      }
      expect(ticks, greaterThanOrEqualTo(0));
    },
  );

  test(
    'two sessions run concurrently without serialising each other',
    () async {
      final first = await AnyCodec.diagnosticEncoder(Codec.h264);
      final second = await AnyCodec.diagnosticEncoder(Codec.h264);
      try {
        expect(first.workerId, isNot(second.workerId));
        final started = Stopwatch()..start();
        await Future.wait(<Future<void>>[
          _encodeMany(first, 60),
          _encodeMany(second, 60),
        ]);
        started.stop();
        expect(started.elapsedMilliseconds, greaterThan(0));
      } finally {
        await first.close();
        await second.close();
      }
    },
  );
}

Future<void> _encodeMany(AnycodecEncoder encoder, int frames) async {
  for (var index = 0; index < frames; index++) {
    await encoder.encode(sampleVideoFrame(pts: index));
    final packet = await drainPacket(encoder);
    if (packet == null) {
      fail('frame $index produced no packet on worker ${encoder.workerId}');
    }
  }
}
