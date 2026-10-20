/// The worker pool: are sessions really spread across isolates, and do they all
/// make progress?
///
/// Two things are being proved here. First, that `AnyCodec.encoder` binds each
/// session to a worker round-robin *at creation time* — two sessions created
/// "simultaneously" must not collide on one thread, because the C ABI's handles
/// are thread-affine. Second, that several sessions on different workers can be
/// driven concurrently without interfering with each other's data.
library;

import 'package:anycodec/anycodec.dart';
import 'package:test/test.dart';

import 'support/frames.dart';

void main() {
  setUpAll(() => AnyCodec.initialize(workers: 3));
  tearDownAll(AnyCodec.shutdown);

  test('initialize(3) gives exactly three workers', () async {
    await AnyCodec.initialize(workers: 3);
    expect(AnyCodec.workerCount, 3);
    // Re-initialising with the same count is a no-op, not an error.
    await AnyCodec.initialize(workers: 3);
    expect(AnyCodec.workerCount, 3);
    // So is asking for "auto" once the pool is up.
    await AnyCodec.initialize();
    expect(AnyCodec.workerCount, 3);
  });

  test(
    'two encoders created in parallel are bound to different workers',
    () async {
      final sessions = await Future.wait(<Future<AnycodecEncoder>>[
        AnyCodec.diagnosticEncoder(Codec.h264),
        AnyCodec.diagnosticEncoder(Codec.h264),
      ]);
      try {
        expect(
          sessions[0].workerId,
          isNot(sessions[1].workerId),
          reason: 'round-robin assignment must happen before any await',
        );
        expect(<int>{sessions[0].workerId, sessions[1].workerId}.length, 2);
        for (final session in sessions) {
          expect(session.workerId, inInclusiveRange(0, 2));
          await session.encode(sampleVideoFrame(pts: 1));
          final packet = await drainPacket(session);
          expect(
            packet,
            isNotNull,
            reason: 'worker ${session.workerId} must work',
          );
          expect(packet!.pts, 1);
        }
      } finally {
        for (final session in sessions) {
          await session.close();
        }
      }
    },
  );

  test(
    'a pool of three spreads more than three sessions round-robin',
    () async {
      final sessions = <AnycodecEncoder>[];
      try {
        for (var index = 0; index < 4; index++) {
          sessions.add(await AnyCodec.diagnosticEncoder(Codec.h264));
        }
        final workers = <int>[for (final session in sessions) session.workerId];
        expect(workers.length, 4);
        expect(
          workers.toSet().length,
          3,
          reason: 'four sessions over three workers: round-robin wraps around',
        );
        expect(
          workers[0],
          workers[3],
          reason: 'the fourth session returns to the first worker',
        );
      } finally {
        for (final session in sessions) {
          await session.close();
        }
      }
    },
  );

  test('three concurrent sessions all make progress', () async {
    await AnyCodec.initialize(workers: 3);
    const sessions = 3;
    const rounds = 12;

    final encoders = <AnycodecEncoder>[];
    final decoders = <AnycodecDecoder>[];
    for (var index = 0; index < sessions; index++) {
      encoders.add(await AnyCodec.diagnosticEncoder(Codec.h264));
      decoders.add(await AnyCodec.diagnosticDecoder(Codec.h264));
    }

    try {
      expect(
        <int>{for (final encoder in encoders) encoder.workerId}.length,
        sessions,
        reason: 'each of the three encoders gets its own worker',
      );

      // All three run at once: every session encodes, receives, decodes and
      // receives again, round after round, and checks its own bytes.
      final results = await Future.wait(<Future<int>>[
        for (var index = 0; index < sessions; index++)
          _roundTrip(encoders[index], decoders[index], index, rounds),
      ]);
      for (final completed in results) {
        expect(completed, rounds);
      }
    } finally {
      for (final encoder in encoders) {
        await encoder.close();
      }
      for (final decoder in decoders) {
        await decoder.close();
      }
    }
  });

  test('closing a session on one worker does not disturb the others', () async {
    final first = await AnyCodec.diagnosticEncoder(Codec.h264);
    final second = await AnyCodec.diagnosticEncoder(Codec.h264);
    try {
      expect(first.workerId, isNot(second.workerId));
      await first.close();
      expect(first.isClosed, isTrue);
      // The second session is on a different isolate and must be untouched.
      await second.encode(sampleVideoFrame(pts: 99));
      final packet = await drainPacket(second);
      expect(packet, isNotNull);
      expect(packet!.pts, 99);
    } finally {
      await first.close();
      await second.close();
    }
  });

  test('the runtime survives a shutdown and starts again', () async {
    // Sessions left open must not stop shutdown, and the runtime must be usable
    // afterwards: the pool is recreated on the next call.
    final leaked = await AnyCodec.diagnosticEncoder(Codec.h264);
    await AnyCodec.shutdown();
    expect(leaked.isClosed, isTrue, reason: 'shutdown closes what was open');
    expect(AnyCodec.workerCount, 0);

    await AnyCodec.initialize(workers: 3);
    expect(AnyCodec.workerCount, 3);
    final encoder = await AnyCodec.diagnosticEncoder(Codec.h264);
    try {
      expect(encoder.workerId, inInclusiveRange(0, 2));
      await encoder.encode(sampleVideoFrame(pts: 5));
      expect(await drainPacket(encoder), isNotNull);
    } finally {
      await encoder.close();
    }
  });
}

/// Encodes and decodes [rounds] pictures on one session pair, returning the
/// number that made it round the loop intact.
Future<int> _roundTrip(
  AnycodecEncoder encoder,
  AnycodecDecoder decoder,
  int session,
  int rounds,
) async {
  var completed = 0;
  for (var round = 0; round < rounds; round++) {
    final pts = session * 100000 + round;
    final frame = sampleVideoFrame(pts: pts);
    await encoder.encode(frame);
    final packet = await drainPacket(encoder);
    if (packet == null) {
      break;
    }
    expect(packet.pts, pts, reason: 'session $session round $round');
    await decoder.decode(packet);
    final decoded = await drainFrame(decoder);
    if (decoded == null) {
      break;
    }
    expect(decoded.pts, pts, reason: 'session $session round $round');
    expect(
      (decoded as VideoFrame).planes[0],
      orderedEquals(frame.planes[0]),
      reason:
          'session $session round $round must not see another session bytes',
    );
    completed++;
  }
  return completed;
}
