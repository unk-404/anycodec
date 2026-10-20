/// PCM stream plumbing and spectrum analysis for AnyCodec.
///
/// This package answers two questions that are *not* the player's job, and
/// deliberately stops there:
///
/// * **"how do I get decoded audio out to whatever plays it?"** —
///   [PcmStreamSource] accepts bytes in any supported encoding and hands back
///   normalised `float32` chunks. It does not open a device. Choosing the device,
///   the buffer size, the audio session and the interruption policy belongs to
///   the platform's audio stack, not here.
/// * **"what data do I draw?"** — [SpectrumAnalyzer] turns that stream into
///   [SpectrumFrame]s: per-bin magnitudes in dBFS and log-spaced band levels
///   already normalised to `0..1`. It renders nothing.
///
/// A player is a loop the caller writes:
///
/// ```dart
/// final source = PcmStreamSource(
///   format: PcmFormat(
///     sampleFormat: PcmSampleFormat.f32,
///     sampleRate: 48000,
///     channels: 2,
///   ),
/// );
/// final analyzer = SpectrumAnalyzer(fftSize: 2048, sampleRate: 48000);
///
/// // Producer: push a decoded audio frame. An `anycodec` AudioFrame bridges in
/// // one line; this package intentionally does not depend on `anycodec`, because
/// // it must also work on the web where the native engine does not exist.
/// source.addBytes(decoderFormat, frameBytes);
///
/// // Consumer: whatever device API you chose asks for the next block.
/// final chunk = await source.readChunk(1024);
/// if (chunk != null) {
///   analyzer.add(chunk, channels: 2);
///   final spectrum = analyzer.analyze(); // magnitudesDb / bandValues
///   device.write(chunk);                 // <- not this package's business
/// }
/// ```
///
/// Pure Dart, no dependencies, no `dart:ffi`: it works identically on native and
/// on the web, which is why it is a separate package from `anycodec`.
library;

export 'src/fft.dart' show FftPlan;
export 'src/pcm_format.dart'
    show
        PcmFormat,
        PcmSampleFormat,
        float32ToPcm,
        mixToMono,
        pcmToFloat32;
export 'src/pcm_ring.dart' show PcmRingBuffer, PcmStreamSource;
export 'src/spectrum.dart'
    show SpectrumAnalyzer, SpectrumFrame, SpectrumWindow;
