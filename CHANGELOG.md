## 0.1.1

Bug-fix release. **The C ABI is unchanged** — `ANYCODEC_ABI_VERSION` is still `1`,
so a build of 0.1.1 is a drop-in replacement for 0.1.0.

### Fixed

* **`flutter build web` failed for every consumer.** `hook/build.dart` read
  `input.config.code` without first checking whether code assets were being built,
  and that property throws when they are not — which is the case for a web build:

  ```
  Bad state: HookConfig.code should only be accessed when building code assets
  ```

  The guard is now `if (!input.config.buildCodeAssets) return;`.

* **H.264 encoding failed on any Linux machine where `openh264enc` is the
  selected encoder.** The raw-video caps handed to `appsrc` were built with
  `gst_video::VideoCapsBuilder`, which silently appends
  `framerate=(fraction)[ 0/1, 2147483647/1 ]`. That is a *range*, so the caps were
  not fixed — and `appsrc` turns `set_caps` into a `gst_event_new_caps`, which
  asserts that its caps are fixed:

  ```
  gst_event_new_caps: assertion 'gst_caps_is_fixed (caps)' failed
  Internal data stream error ... reason not-negotiated (-4)
  ```

  `x264enc` tolerates an unfixed `framerate`; `openh264enc` refuses to negotiate.
  The same build therefore worked on one Linux box and failed on another, which is
  what made it look like an environment difference rather than a bug. The caps are
  now built with `gst::Caps::builder`, and
  `native::linux::elements::tests::every_caps_we_hand_to_an_appsrc_is_fixed`
  asserts the invariant that was missing.
* **Nine `unresolved doc reference` warnings**, all from
  `lib/anycodec_platform.dart`. That library is a `library;` barrel with
  conditional exports, so the names living in one platform branch cannot be
  linked from a platform-neutral library. dartdoc reported all nine against it.
* **`clippy::print_literal` in `rust/examples/capabilities.rs`**, which made
  `cargo clippy --all-targets -- -D warnings` fail.

### Packaging

* **`LICENSE` was a placeholder** (`TODO: Add your license here.`). It is now the
  MIT licence, which 0.1.0 shipped without.
* **`pubspec.yaml`'s description was the template default**
  (`"A new Dart FFI package project."`) — the text pub.dev renders in search
  results. It now describes the package.
* **Added `.pubignore`.** `dart pub publish` falls back to `.gitignore` when it is
  absent, and `.gitignore` does not exclude `.agents/` (5 GB of agent workspace in
  this repository) or `rust/target/` (3.3 GB). The published archive is 0.6 MB.
* **Repaired UTF-8 damage in 23 source files and removed 36 byte-order marks.**
  A PowerShell `Set-Content -Encoding utf8` round trip had re-encoded every em
  dash into two CJK code points and prepended a BOM.
* `rust/Cargo.lock` is now published, so the dependency versions resolved when the
  cdylib is built do not drift under a consumer's feet.

### Repository

Not shipped to pub.dev (`.github/` and `.agents/` are excluded from the archive),
but they are why this release is worth having:

* `.github/workflows/ci.yml` — the Rust suite on Linux, Windows and macOS, the Dart
  binding tests on all three, `cargo check` for the three Android and four Apple
  targets, a dependency-floor job, and two publish-readiness gates.
* `.github/scripts/check_archive.dart` — asserts the published archive is complete
  by resolving every relative import in it, which is the failure that makes a
  package publish cleanly and compile for nobody.
* `.github/scripts/validate_workflows.dart` — checks the workflow structure
  without pushing it.
* `.agents/scripts/preflight.ps1` — runs the same checks locally, unfiltered.

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
