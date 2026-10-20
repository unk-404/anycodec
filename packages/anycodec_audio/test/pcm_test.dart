import 'dart:typed_data';

import 'package:anycodec_audio/anycodec_audio.dart';
import 'package:test/test.dart';

PcmFormat _mono(PcmSampleFormat format) =>
    PcmFormat(sampleFormat: format, sampleRate: 48000, channels: 1);

PcmFormat _stereo(PcmSampleFormat format) =>
    PcmFormat(sampleFormat: format, sampleRate: 48000, channels: 2);

Float32List _samples(List<double> values) => Float32List.fromList(values);

void main() {
  group('sample conversion', () {
    test('s16 scales to the full signed range', () {
      final bytes = Uint8List(10);
      final view = ByteData.sublistView(bytes);
      const values = [-32768, -16384, 0, 16384, 32767];
      for (var i = 0; i < values.length; i++) {
        view.setInt16(i * 2, values[i], Endian.little);
      }
      final f = pcmToFloat32(bytes, _mono(PcmSampleFormat.s16));
      expect(f[0], -1.0);
      expect(f[1], -0.5);
      expect(f[2], 0.0);
      expect(f[3], 0.5);
      expect(f[4], closeTo(1.0, 1e-4));
    });

    test('u8 is un-biased at 128', () {
      final f = pcmToFloat32(
        Uint8List.fromList([0, 128, 255]),
        _mono(PcmSampleFormat.u8),
      );
      expect(f[0], -1.0);
      expect(f[1], 0.0);
      expect(f[2], closeTo(1.0, 1e-2));
    });

    test('f32 passes through untouched', () {
      final source = _samples([0.25, -0.5]);
      final bytes = float32ToPcm(source, _mono(PcmSampleFormat.f32));
      final back = pcmToFloat32(bytes, _mono(PcmSampleFormat.f32));
      expect(back[0], closeTo(0.25, 1e-7));
      expect(back[1], closeTo(-0.5, 1e-7));
    });

    test('float outside the range is clamped, not wrapped', () {
      // Wrapping here would turn a loud peak into loud noise of opposite sign.
      final bytes = float32ToPcm(
        _samples([2.0, -2.0]),
        _mono(PcmSampleFormat.s16),
      );
      final back = pcmToFloat32(bytes, _mono(PcmSampleFormat.s16));
      expect(back[0], closeTo(1.0, 1e-3));
      expect(back[1], closeTo(-1.0, 1e-3));
    });

    test('a partial frame is rejected rather than silently shifted', () {
      expect(
        () => pcmToFloat32(Uint8List(3), _stereo(PcmSampleFormat.s16)),
        throwsArgumentError,
      );
    });

    test('mixToMono averages instead of summing', () {
      final mono = mixToMono(
        _samples([1.0, 0.0, 0.0, 1.0, 0.5, 0.5]),
        2,
      );
      expect(mono, [0.5, 0.5, 0.5]);
    });

    test('format rejects nonsense geometry', () {
      expect(
        () => PcmFormat(
          sampleFormat: PcmSampleFormat.s16,
          sampleRate: 0,
          channels: 2,
        ),
        throwsArgumentError,
      );
      expect(
        () => PcmFormat(
          sampleFormat: PcmSampleFormat.s16,
          sampleRate: 48000,
          channels: 0,
        ),
        throwsArgumentError,
      );
    });

    test('bytesPerFrame accounts for the channel count', () {
      expect(_stereo(PcmSampleFormat.s16).bytesPerFrame, 4);
      expect(_stereo(PcmSampleFormat.f32).bytesPerFrame, 8);
      expect(_mono(PcmSampleFormat.u8).bytesPerFrame, 1);
    });
  });

  group('PcmRingBuffer', () {
    test('preserves order across a wrap-around', () {
      final ring = PcmRingBuffer(
        format: _mono(PcmSampleFormat.f32),
        capacityFrames: 4,
      );
      expect(ring.write(_samples([1, 2, 3, 4])), 4);
      final first = Float32List(2);
      expect(ring.read(first), 2);
      expect(first, [1, 2]);
      expect(ring.write(_samples([5, 6])), 2);
      final rest = Float32List(4);
      expect(ring.read(rest), 4);
      expect(rest, [3, 4, 5, 6]);
    });

    test('a full buffer refuses a short write by default', () {
      final ring = PcmRingBuffer(
        format: _mono(PcmSampleFormat.f32),
        capacityFrames: 4,
      );
      expect(ring.write(_samples([1, 2, 3, 4, 5, 6])), 4);
      expect(ring.droppedFrames, 0, reason: 'nothing was lost, it was refused');
      expect(ring.freeFrames, 0);
      final out = Float32List(4);
      ring.read(out);
      expect(out, [1, 2, 3, 4]);
    });

    test('dropOldest keeps the newest audio and counts the loss', () {
      final ring = PcmRingBuffer(
        format: _mono(PcmSampleFormat.f32),
        capacityFrames: 4,
        dropOldest: true,
      );
      expect(ring.write(_samples([1, 2, 3, 4, 5, 6])), 6);
      expect(ring.droppedFrames, 2);
      final out = Float32List(4);
      ring.read(out);
      expect(out, [3, 4, 5, 6]);
    });

    test('readOrSilence always returns a full buffer', () {
      final ring = PcmRingBuffer(
        format: _mono(PcmSampleFormat.f32),
        capacityFrames: 8,
      );
      ring.write(_samples([1, 2]));
      expect(ring.readOrSilence(4), [1, 2, 0, 0]);
    });

    test('an underrun reads what exists and reports the count', () {
      final ring = PcmRingBuffer(
        format: _stereo(PcmSampleFormat.f32),
        capacityFrames: 8,
      );
      ring.write(_samples([1, 2, 3, 4])); // two stereo frames
      final out = Float32List(8);
      expect(ring.read(out), 2);
      expect(out.sublist(0, 4), [1, 2, 3, 4]);
      expect(out.sublist(4), [0, 0, 0, 0]);
    });

    test('fillRatio and clear behave', () {
      final ring = PcmRingBuffer(
        format: _mono(PcmSampleFormat.f32),
        capacityFrames: 4,
      );
      ring.write(_samples([1, 2]));
      expect(ring.fillRatio, 0.5);
      ring.clear();
      expect(ring.availableFrames, 0);
      expect(ring.fillRatio, 0);
    });
  });

  group('PcmStreamSource', () {
    test('converts on the way in and hands back float32', () {
      final source = PcmStreamSource(format: _stereo(PcmSampleFormat.f32));
      final bytes = Uint8List(8);
      final view = ByteData.sublistView(bytes);
      view.setInt16(0, 32767, Endian.little);
      view.setInt16(2, -32768, Endian.little);
      view.setInt16(4, 16384, Endian.little);
      view.setInt16(6, 0, Endian.little);
      source.addBytes(_stereo(PcmSampleFormat.s16), bytes);
      final out = Float32List(4);
      expect(source.read(out), 2);
      expect(out[0], closeTo(1.0, 1e-4));
      expect(out[1], -1.0);
      expect(out[2], 0.5);
      expect(out[3], 0.0);
    });

    test('refuses a sample-rate mismatch instead of silently resampling', () {
      final source = PcmStreamSource(format: _stereo(PcmSampleFormat.f32));
      expect(
        () => source.addBytes(
          PcmFormat(
            sampleFormat: PcmSampleFormat.s16,
            sampleRate: 44100,
            channels: 2,
          ),
          Uint8List(4),
        ),
        throwsArgumentError,
      );
    });

    test('refuses a channel-count mismatch', () {
      final source = PcmStreamSource(format: _stereo(PcmSampleFormat.f32));
      expect(
        () => source.addBytes(_mono(PcmSampleFormat.s16), Uint8List(2)),
        throwsArgumentError,
      );
    });

    test('readChunk waits for enough data', () async {
      final source = PcmStreamSource(format: _mono(PcmSampleFormat.f32));
      final pending = source.readChunk(4);
      var done = false;
      pending.then((_) => done = true);
      await Future<void>.delayed(Duration.zero);
      expect(done, isFalse, reason: 'nothing has been added yet');
      source.addFloat32(_samples([1, 2, 3, 4]));
      final chunk = await pending;
      expect(chunk, [1, 2, 3, 4]);
    });

    test('close completes pending reads with null', () async {
      final source = PcmStreamSource(format: _mono(PcmSampleFormat.f32));
      final pending = source.readChunk(4);
      source.close();
      expect(await pending, isNull);
      expect(source.isClosed, isTrue);
    });

    test('adding to a closed source is an error', () {
      final source = PcmStreamSource(format: _mono(PcmSampleFormat.f32));
      source.close();
      expect(() => source.addFloat32(_samples([1])), throwsStateError);
    });

    test('chunks() ends when the source closes', () async {
      final source = PcmStreamSource(format: _mono(PcmSampleFormat.f32));
      final collected = source.chunks(framesPerChunk: 2).toList();
      source.addFloat32(_samples([1, 2, 3, 4]));
      await Future<void>.delayed(Duration.zero);
      source.close();
      final chunks = await collected;
      expect(chunks, [
        [1.0, 2.0],
        [3.0, 4.0],
      ]);
    });
  });
}
