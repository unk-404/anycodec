## 0.0.1

The Dart/Flutter layer over the frozen AnyCodec C ABI (revision 1).

* Unified `AnyCodec` API (`package:anycodec/anycodec.dart`): `initialize`,
  `shutdown`, `capabilities`, `probeHardware`, `capabilityReport`,
  `platformReport`, `encoder`, `decoder`, `diagnosticEncoder`,
  `diagnosticDecoder`, `passthroughCodecId`, `workerCount`. Every operation is
  `Future`-based on every platform.
* A pool of worker isolates owns every native handle; sessions are bound to one
  worker round-robin, so several run in parallel and no native call ever happens
  on the calling isolate. Frames cross the boundary as `TransferableTypedData`.
* Software-fallback reporting through `CodecPath` (`isSoftware`,
  `hardwareAccelerated`, `detail`, `fallbackReason`) and through the capability
  report (`CodecCapability`, `BackendCapability`), with `BackendPreference` to
  refuse a fallback up front.
* `AnyCodecException` / `AnyCodecErrorKind`, mapped one-to-one onto the ABI's
  `ANYCODEC_STATUS_*` codes, with the Rust-side message preserved.
* A diagnostic codec (`diagnosticEncoder` / `diagnosticDecoder`) for proving the
  pipeline — timestamps, ownership, the isolate hand-off, flush — on any host.
* A WebCodecs runtime for the browser, plus a pure-Dart implementation of the
  diagnostic codec that speaks the exact same wire format as the Rust one.
* Platform-specific APIs in `package:anycodec/anycodec_platform.dart`:
  `NativeLibraryInfo` and `LastNativeError` on native, `WebCodecs` and
  `WebCodecsSupport` on the web, selected by conditional export so `dart:ffi`
  never reaches a web build and `dart:js_interop` never reaches a native one.
* Tests: ABI reachability, struct layout and constants, error mapping,
  capability reporting, a real encode/decode round trip (including a real FLAC
  encode and a byte-for-byte comparison against the Rust diagnostic codec), the
  worker pool's round-robin assignment, and a non-blocking proof.
