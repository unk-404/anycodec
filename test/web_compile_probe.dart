// A compile probe for the browser branch of the conditional imports.
//
// This is NOT a test: it has no `main` expectations and its name does not match
// `*_test.dart`, so `dart test` never runs it. Its job is to make
// `dart compile js` build the *web* runtime — `runtime_web.dart`, `platform_web.dart`
// and every `dart:js_interop` / `package:web` call inside them — which a
// VM-only machine cannot exercise any other way:
//
//   dart compile js test/web_compile_probe.dart -o .dart_tool/web_compile_probe.js
//
// It names `WebCodecs` (a web-only export of `anycodec_platform.dart`) on
// purpose, so tree shaking cannot drop the WebCodecs code before type checking.
// The VM analyzer resolves conditional imports for `dart:io` and therefore
// cannot see that name, so this one file is excluded in `analysis_options.yaml`;
// `dart compile js` is what verifies it instead.
import 'package:anycodec/anycodec.dart';
import 'package:anycodec/anycodec_platform.dart';

void main() {
  // The unified API on the web runtime.
  print('workers: ${AnyCodec.workerCount}');
  print('passthrough codec id: ${AnyCodec.passthroughCodecId}');
  print('webcodec string for h264: ${webCodecStringFor(Codec.h264)}');
  print('web pixel format for i420: ${webPixelFormatFor(PixelFormat.i420)}');
  print('webcodecs video supported: ${WebCodecs.videoSupported}');
  print('webcodecs audio supported: ${WebCodecs.audioSupported}');
  print('hardware hint: ${webHardwareHint(BackendPreference.requireHardware)}');

  AnyCodec.capabilities().then((capabilities) {
    print('capabilities: ${capabilities.length}');
  });
  WebCodecs.supportsVideoEncoder(
    codec: Codec.vp8,
    width: 320,
    height: 240,
  ).then((support) => print('vp8 encoder: ${support.note}'));
  WebCodecs.capabilities().then((support) {
    print('webcodecs codecs: ${support.length}');
  });
  AnyCodec.diagnosticEncoder(Codec.h264).then((encoder) async {
    print('diagnostic path: ${encoder.path.summary}');
    await encoder.close();
  });
}
