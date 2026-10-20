import 'dart:async';
import 'dart:typed_data';

import 'pcm_format.dart';

/// A fixed-capacity FIFO of interleaved `float32` frames.
///
/// This is the buffer a player sits between its decoder and its device callback:
/// the decoder writes as frames arrive, the callback reads exactly what it needs,
/// and the two never allocate in between. Everything here is synchronous and
/// allocation-free after construction, because the consumer is usually an audio
/// callback where allocating is not allowed.
///
/// Storage is normalised `float32` regardless of the source encoding — see
/// `pcmToFloat32`. That keeps the read path branch-free, which is the path that
/// runs on the real-time thread.
final class PcmRingBuffer {
  /// Creates a ring with room for [capacityFrames] frames.
  ///
  /// [dropOldest] decides the overflow policy, and the two policies are not
  /// interchangeable:
  ///
  /// * `true` — a *live* source (a capture device, a visualiser). New audio
  ///   matters more than old audio, so the oldest frames are overwritten and
  ///   [droppedFrames] counts how much was lost.
  /// * `false` — *playback*. Losing audio silently is worse than telling the
  ///   producer to slow down, so a full buffer accepts nothing and the producer
  ///   sees a short write.
  PcmRingBuffer({
    required this.format,
    required this.capacityFrames,
    this.dropOldest = false,
  }) : assert(capacityFrames > 0, 'capacity must be positive'),
       _data = Float32List(capacityFrames * format.channels);

  /// The layout of the samples this buffer holds.
  final PcmFormat format;

  /// How many frames fit.
  final int capacityFrames;

  /// The overflow policy. See the constructor.
  final bool dropOldest;

  final Float32List _data;
  int _read = 0;
  int _write = 0;
  int _stored = 0;
  int _dropped = 0;

  /// Frames currently readable.
  int get availableFrames => _stored;

  /// Frames that can be written before the buffer is full.
  int get freeFrames => capacityFrames - _stored;

  /// Frames lost to [dropOldest] overflow since construction.
  ///
  /// A player should surface this: a non-zero value means the producer is
  /// running ahead of the device, which is a real defect worth noticing rather
  /// than a number to ignore.
  int get droppedFrames => _dropped;

  /// How many frames are buffered, as a fraction of capacity.
  double get fillRatio => _stored / capacityFrames;

  /// Discards everything buffered. Does not reset [droppedFrames].
  void clear() {
    _read = 0;
    _write = 0;
    _stored = 0;
  }

  /// Writes interleaved [samples], returning how many whole frames were stored.
  ///
  /// A partial frame at the end of [samples] is ignored. With `dropOldest` the
  /// write always succeeds in full and the overwritten frames are counted; with
  /// the default policy it may store fewer frames than offered.
  int write(Float32List samples) {
    final channels = format.channels;
    final frames = samples.length ~/ channels;
    if (frames == 0) {
      return 0;
    }
    var toWrite = frames;
    if (toWrite > freeFrames) {
      if (!dropOldest) {
        toWrite = freeFrames;
      } else {
        // Drop from the front so the newest audio survives.
        final overflow = toWrite - freeFrames;
        _read = (_read + overflow) % capacityFrames;
        _stored -= overflow;
        _dropped += overflow;
      }
    }
    for (var i = 0; i < toWrite; i++) {
      final src = i * channels;
      final dst = _write * channels;
      for (var channel = 0; channel < channels; channel++) {
        _data[dst + channel] = samples[src + channel];
      }
      _write = (_write + 1) % capacityFrames;
    }
    _stored += toWrite;
    return toWrite;
  }

  /// Reads up to [dest.length ~/ channels] frames into [dest].
  ///
  /// Returns the number of whole frames read; the tail of [dest] beyond that is
  /// left untouched. Reading fewer frames than requested is normal and is not an
  /// error — an underrun is a scheduling fact, not a failure.
  int read(Float32List dest) {
    final channels = format.channels;
    final wanted = dest.length ~/ channels;
    final toRead = wanted < _stored ? wanted : _stored;
    for (var i = 0; i < toRead; i++) {
      final src = _read * channels;
      final dst = i * channels;
      for (var channel = 0; channel < channels; channel++) {
        dest[dst + channel] = _data[src + channel];
      }
      _read = (_read + 1) % capacityFrames;
    }
    _stored -= toRead;
    return toRead;
  }

  /// Reads [frames] frames, padding with silence if fewer are available.
  ///
  /// This is the shape a device callback needs: it must always be handed a
  /// full buffer, and silence is the only honest filler for a hole.
  Float32List readOrSilence(int frames) {
    final dest = Float32List(frames * format.channels);
    read(dest);
    return dest;
  }

  @override
  String toString() =>
      'PcmRingBuffer(${format.sampleFormat.name} ${format.sampleRate}Hz '
      'x${format.channels}, $_stored/$capacityFrames frames'
      '${_dropped > 0 ? ', $_dropped dropped' : ''})';
}

/// The producer side of an audio pipeline: accepts PCM in any supported encoding
/// and exposes it as a stream of `float32` chunks.
///
/// This is the "standard audio stream" the package promises. It is deliberately
/// **not** a player: it does not open a device, does not know about a sample
/// clock, and has no opinion about buffering strategy beyond the ring it owns.
/// A player is a loop that calls [readChunk] and hands the result to whatever
/// device API it uses.
final class PcmStreamSource {
  /// Creates a source buffering [capacityFrames] frames.
  PcmStreamSource({
    required this.format,
    int capacityFrames = 8192,
    bool dropOldest = false,
  }) : buffer = PcmRingBuffer(
         format: format,
         capacityFrames: capacityFrames,
         dropOldest: dropOldest,
       );

  /// The format this source produces. [add] converts into it.
  final PcmFormat format;

  /// The ring the source writes into and readers drain.
  final PcmRingBuffer buffer;

  final List<_Waiter> _waiters = [];
  bool _closed = false;

  /// `true` once [close] has run.
  bool get isClosed => _closed;

  /// Bytes that would be produced by one frame.
  int get bytesPerFrame => format.bytesPerFrame;

  /// Accepts raw interleaved [bytes] in [sourceFormat] and normalises them.
  ///
  /// Throws [ArgumentError] when `sourceFormat` has a different channel count or
  /// sample rate than [format]. Converting a rate is resampling, which is a
  /// different job with a different quality trade-off, and doing it silently here
  /// would hide a producer bug behind a plausible-sounding output.
  void addBytes(PcmFormat sourceFormat, Uint8List bytes) {
    if (sourceFormat.channels != format.channels) {
      throw ArgumentError.value(
        sourceFormat.channels,
        'sourceFormat.channels',
        'does not match this source (${format.channels})',
      );
    }
    if (sourceFormat.sampleRate != format.sampleRate) {
      throw ArgumentError.value(
        sourceFormat.sampleRate,
        'sourceFormat.sampleRate',
        'different sample rates need a resampler, not a conversion '
            '(this source is ${format.sampleRate}Hz)',
      );
    }
    addFloat32(pcmToFloat32(bytes, sourceFormat));
  }

  /// Accepts already-normalised interleaved `float32`.
  ///
  /// Returns how many frames were stored; with the default (rejecting) overflow
  /// policy this can be fewer than offered, and [PcmRingBuffer.droppedFrames]
  /// stays zero because nothing was lost — the producer was simply too early.
  int addFloat32(Float32List samples) {
    if (_closed) {
      throw StateError('cannot add to a closed PcmStreamSource');
    }
    final written = buffer.write(samples);
    if (written > 0) {
      _wake();
    }
    return written;
  }

  /// Reads up to `dest.length ~/ channels` frames, or returns 0 immediately.
  int read(Float32List dest) => buffer.read(dest);

  /// Waits until [frames] frames are available, then reads exactly that many.
  ///
  /// Completes with `null` once the source is [close]d and drained, which is how
  /// a player knows to stop its loop rather than spin on an empty buffer.
  Future<Float32List?> readChunk(int frames) {
    if (frames <= 0) {
      throw ArgumentError.value(frames, 'frames', 'must be positive');
    }
    if (_closed && buffer.availableFrames == 0) {
      return Future<Float32List?>.value(null);
    }
    if (buffer.availableFrames >= frames) {
      final out = Float32List(frames * format.channels);
      buffer.read(out);
      return Future<Float32List?>.value(out);
    }
    final completer = Completer<Float32List?>();
    _waiters.add(_Waiter(frames, completer));
    return completer.future;
  }

  /// A stream view of [readChunk], for callers that prefer `await for`.
  ///
  /// Completes when the source is closed and drained.
  Stream<Float32List> chunks({required int framesPerChunk}) async* {
    while (true) {
      final chunk = await readChunk(framesPerChunk);
      if (chunk == null) {
        return;
      }
      yield chunk;
    }
  }

  /// Stops accepting data and completes every pending read with `null`.
  ///
  /// Audio already buffered is *discarded*: a closed source means playback is
  /// over, and draining it would make "close" take as long as the buffer.
  void close() {
    if (_closed) {
      return;
    }
    _closed = true;
    buffer.clear();
    for (final waiter in _waiters) {
      if (!waiter.completer.isCompleted) {
        waiter.completer.complete(null);
      }
    }
    _waiters.clear();
  }

  /// Satisfies every waiter whose request can now be met, in request order.
  void _wake() {
    while (_waiters.isNotEmpty) {
      final waiter = _waiters.first;
      if (buffer.availableFrames < waiter.frames) {
        return;
      }
      _waiters.removeAt(0);
      if (waiter.completer.isCompleted) {
        continue;
      }
      final out = Float32List(waiter.frames * format.channels);
      buffer.read(out);
      waiter.completer.complete(out);
    }
  }

  @override
  String toString() =>
      'PcmStreamSource(${format.sampleFormat.name} ${format.sampleRate}Hz '
      'x${format.channels}, ${buffer.availableFrames} frames buffered'
      '${_closed ? ', closed' : ''})';
}

final class _Waiter {
  _Waiter(this.frames, this.completer);
  final int frames;
  final Completer<Float32List?> completer;
}
