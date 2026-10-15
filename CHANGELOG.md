## 0.1.0

First release. A cross-platform audio/video codec engine for Dart and Flutter:
the platform's own media framework is preferred, and a software fallback is
always available and always reported.

### Codecs

Video H.264, H.265, H.266, VP8, VP9, AV1; audio MP3, AAC, Vorbis, Opus, FLAC,
ALAC.

### Platform-native backends

* **Android** — MediaCodec through the `ndk` crate, with real `avcC`/`hvcC` to
  Annex-B conversion, flexible-YUV input layout, crop-rect-aware decoding, and
  hardware detection from the component name.
* **Windows** — Media Foundation, discovered by `MFTEnumEx` rather than by
  hard-coded CLSIDs. Synchronous MFTs only, so `hardwareAccelerated` is reported
  as `false` and says so, instead of claiming a GPU it does not use.
* **macOS / iOS** — VideoToolbox and AudioToolbox through the `objc2` crates.
  Hardware use is read back off the live session
  (`kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder`,
  `VTIsHardwareDecodeSupported`), never assumed.
* **Linux** — GStreamer through `gstreamer-rs`, with an explicit per-codec
  element table and `hardwareAccelerated` true only for the `va*`/`nv*`/`v4l2*`/
  `msdk*` element that was actually selected.
* **Web** — WebCodecs, implemented in Dart (`lib/src/runtime/runtime_web.dart`).

### Software fallback

Pure-Rust codecs, compiled in unconditionally, needing no C toolchain, no cmake
and no nasm: MP3, AAC, FLAC, Opus and VP9 in both directions; H.265 and ALAC
decoding; Vorbis through `symphonia`. A runtime-loaded `libavcodec` tier is
probed and reported honestly when it is absent.

### Knowing which one you got

`CodecPath` answers it: `backend`, `implementation`, `hardwareAccelerated`,
`detail` (the concrete object in use) and `fallbackReason` (every backend that
was skipped and why). `AnyCodec.capabilityReport()` renders the same information
for every codec and direction, and `BackendPreference` lets a caller refuse a
software fallback up front.

### Dart API

* One unified `Future`-based API on every platform
  (`package:anycodec/anycodec.dart`).
* A pool of worker isolates owns every native handle, so no native call happens
  on the calling isolate and several sessions run in parallel. Frames and packets
  cross the boundary as `TransferableTypedData`.
* Platform-specific APIs in `package:anycodec/anycodec_platform.dart`, selected
  by conditional export, so `dart:ffi` never reaches a web build and
  `dart:js_interop` never reaches a native one.
* `AnyCodecException` / `AnyCodecErrorKind` map one-to-one onto the ABI's
  `ANYCODEC_STATUS_*` codes, preserving the Rust-side message.
* A diagnostic codec (`diagnosticEncoder` / `diagnosticDecoder`) for exercising
  the whole pipeline — timestamps, ownership, the isolate hand-off, flush — on a
  host with no media framework at all. The Dart implementation is byte-compatible
  with the Rust one, and a test proves it.
