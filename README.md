# anycodec

Cross-platform audio/video codec sessions for Dart and Flutter, on top of a
frozen C ABI (`rust/include/anycodec.h`). One API, three runtimes, chosen
automatically:

| Platform | Runtime | How it works |
|----------|---------|--------------|
| Android, iOS, macOS, Windows, Linux, plain Dart on the VM | native | a pool of worker isolates, each owning native handles created through the C ABI |
| Browsers | web | WebCodecs, driven directly (`dart:isolate` does not exist there) |
| anything else | stub | every call throws `AnyCodecException` instead of failing to link |

The answering of *"did my encode silently fall back to software?"* is a
first-class feature, not an afterthought: every session reports a `CodecPath`.

## Quick start

```dart
import 'package:anycodec/anycodec.dart' as anycodec;

Future<void> main() async {
  await anycodec.AnyCodec.initialize(); // workers: 0 == auto (cpuCount - 1)

  // A working codec that needs no platform support at all.
  final encoder = await anycodec.AnyCodec.diagnosticEncoder(anycodec.Codec.h264);
  final decoder = await anycodec.AnyCodec.diagnosticDecoder(anycodec.Codec.h264);

  final frame = anycodec.VideoFrame.i420(
    width: 64,
    height: 64,
    y: yPlane,
    u: uPlane,
    v: vPlane,
    pts: 33333, // microseconds
  );

  await encoder.encode(frame);
  final packet = await encoder.receive();
  await decoder.decode(packet!);
  final decoded = await decoder.receive() as anycodec.VideoFrame;

  await encoder.close();
  await decoder.close();
  await anycodec.AnyCodec.shutdown();
}
```

Every member is `static` and every operation is `Future`-based, so native
(isolate) and web (WebCodecs) code have identical signatures and neither can
block the caller's event loop.

### The unified API (`package:anycodec/anycodec.dart`)

```dart
abstract final class AnyCodec {
  static Future<void> initialize({int workers = 0});   // 0 == auto
  static Future<void> shutdown();
  static Future<List<CodecCapability>> capabilities();
  static Future<List<HardwareDevice>> probeHardware();  // never on the UI thread
  static Future<String> capabilityReport();
  static Future<String> platformReport();               // platform-specific extras
  static Future<AnycodecEncoder> encoder(EncoderConfig config,
      {BackendPreference preference = BackendPreference.auto});
  static Future<AnycodecDecoder> decoder(DecoderConfig config,
      {BackendPreference preference = BackendPreference.auto});
  static Future<AnycodecEncoder> diagnosticEncoder(Codec codec);
  static Future<AnycodecDecoder> diagnosticDecoder(Codec codec);
  static int get passthroughCodecId;
  static int get workerCount;
}
```

`AnycodecEncoder` / `AnycodecDecoder` are the live sessions:

```dart
abstract interface class AnycodecEncoder {
  Codec get codec;
  CodecPath get path;          // isSoftware / hardwareAccelerated / fallbackReason
  int get workerId;            // which isolate owns this session (0 on web)
  bool get isClosed;
  Future<void> encode(Frame frame);
  Future<Packet?> receive();   // null == "needs more input"
  Future<List<Packet>> flush();
  Future<int> bufferedPackets();
  Future<void> setBitrate(int bitsPerSecond);
  Future<void> requestKeyframe();
  Future<Uint8List?> extradata();
  Future<void> close();        // idempotent
}
```

## Software-fallback reporting

`CodecPath` is the answer to "what actually did the work?":

```dart
final encoder = await anycodec.AnyCodec.encoder(
  anycodec.EncoderConfig.video(codec: anycodec.Codec.h264, width: 1920, height: 1080, bitrate: 6000000),
);

print(encoder.path.summary);
// mediafoundation (native, hardware): mediafoundation: h264 encoder (synchronous MFT)
// or
// rust (software): rust: passthrough (verbatim store)

if (encoder.path.isSoftware) {
  print('fell back to software because: ${encoder.path.fallbackReason}');
  // e.g. "mediafoundation: HEVC Video Extensions are not installed; ffmpeg: ..."
}

// Fields: backend, implementation, hardwareAccelerated, detail, fallbackReason.
```

`BackendPreference` decides what is acceptable before that happens:

* `auto` — platform frameworks first, software fallback after;
* `nativeOnly` — refuse the fallback (the failure is `unavailable`, and the
  message names every backend that was tried);
* `softwareOnly` — never touch a platform framework;
* `requireHardware` — only implementations known to run on dedicated hardware.

`AnyCodec.capabilities()` gives the same information *before* you start: for each
of the twelve codecs and both directions, every registered backend, whether it is
available right now, and why not.

## Platform-specific APIs

`package:anycodec/anycodec_platform.dart` is a conditional library: `dart:ffi`
never leaks into a web build and `dart:js_interop` never leaks into a native one.

**Native.** The loaded library and the thread-local error channel:

```dart
import 'package:anycodec/anycodec_platform.dart';

final info = NativeLibraryInfo.read();
print('${info.versionString}, abi ${info.abiVersion}, asset ${info.assetId}');
print(info.libraryPath ?? '(the native-asset toolchain does not expose it)');

final lastError = LastNativeError.read();
if (!lastError.isOk) {
  print('${lastError.kind.token}: ${lastError.message}');
}
LastNativeError.clear();

print(nativePassthroughCodecId()); // the diagnostic codec's internal slot
```

**Web.** WebCodecs capability and configuration probing:

```dart
import 'package:anycodec/anycodec_platform.dart';

print(WebCodecs.videoSupported);  // VideoEncoder/VideoDecoder exist
print(WebCodecs.audioSupported);  // false: package:web 1.1.1 has video only

final support = await WebCodecs.supportsVideoEncoder(
  codec: Codec.vp9,
  width: 1920,
  height: 1080,
);
print('${support.supported} ${support.codecString} ${support.note}');

for (final entry in await WebCodecs.capabilities()) {
  print('${entry.codec.name}: encode=${entry.videoEncode} decode=${entry.videoDecode}');
}
```

UI code that has to compile for every platform can use
`AnyCodec.platformReport()`, which renders the same information as text without
naming a conditionally exported class.

**Everywhere.** `CodecPath`, `HardwareDevice`, `HardwareDeviceCodec`,
`CodecCapability`, `BackendCapability`, `Backend`, `Implementation`,
`BackendPreference`, `Codec`, `CodecDirection`, `PixelFormat`, `SampleFormat`,
`ChannelLayout`, `Packet`, `PacketFlags`, `EncoderConfig`, `DecoderConfig`,
`AnyCodecException`.

## Non-blocking, multi-threaded

* A pool of `N` long-lived worker isolates, each of which opens and owns native
  handles for its whole life. The pool is lazy: created on first use, released by
  `AnyCodec.shutdown()`.
* Every session is bound to one worker **round-robin at creation time**, so
  several sessions genuinely run in parallel, and each native handle is only ever
  touched by the thread that created it (which is what the C ABI requires).
* The calling isolate never issues a native call — not even
  `anycodec_probe_devices`, which the header itself flags as slow and
  GPU-touching.
* Frames cross the isolate boundary as `TransferableTypedData`: the bytes are
  copied once into a private transport buffer on the sending side and then moved,
  so the receiving isolate owns bytes nobody else can observe or mutate. Your
  `Uint8List`s stay valid.
* A worker that dies (a Rust panic, a `SIGSEGV`, a kill) fails every request in
  flight with an `AnyCodecException` — never a hang.

`test/non_blocking_test.dart` proves the point: a 5 ms `Timer` keeps firing while
200 frames are encoded, and the longest gap between ticks stays small.

## Errors

Failures are `AnyCodecException`, classified by `AnyCodecErrorKind`, whose
numeric values are the ABI's `ANYCODEC_STATUS_*` codes:

| kind | status | meaning |
|------|--------|---------|
| `invalidArgument` | -1 | a malformed argument (NULL, out of range, bad geometry) |
| `unsupported` | -2 | this build does not implement it |
| `unavailable` | -3 | implemented, but nothing on this machine can run it |
| `backend` | -4 | the platform API reported a failure |
| `state` | -5 | the object is not in a state that permits the call (e.g. closed) |
| `outOfMemory` | -6 | allocation failed |
| `io` | -7 | a file or stream operation failed |
| `internal` | -8 | an invariant was violated, or a Rust panic was caught — a bug |

## Timebase

`pts`, `dts` and `duration` are **microseconds** everywhere, on every platform.
The C ABI is timebase-agnostic, so the Dart layer picks one unit rather than
letting each backend invent its own.

## Testing

```sh
dart analyze                                   # zero issues
dart format --output=none --set-exit-if-changed lib test example/lib
dart test                                      # 61 tests; the web ones skip on the VM
cd example && flutter analyze
```

`dart test` needs no Flutter: the build hook (see `hook/build.dart`) compiles
`rust/` into a `cdylib` and hands it to `@ffi.Native` as a code asset on the first
run. The web runtime is verified by compiling it:

```sh
dart compile js test/web_compile_probe.dart -o .dart_tool/web_probe/probe.js
```

and the browser-only suite runs with:

```sh
dart test -p chrome test/web_runtime_test.dart
```

## Regenerating the bindings

`lib/src/bindings/anycodec_bindings_generated.dart` is generated; never edit it by
hand. After a change to the C ABI (and only then):

```sh
cd rust
cbindgen --config cbindgen.toml --crate anycodec --output include/anycodec.h
cd ..
dart run ffigen --config ffigen.yaml
```

`dart:ffi` resolves a mismatching ABI revision into a thrown `AnyCodecException`
rather than memory corruption: `AnyCodec.initialize()` compares
`anycodec_abi_version()` against the revision the bindings were generated for.

## Licence

See `LICENSE`.
