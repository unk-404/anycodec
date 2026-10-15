/// The runtime used on platforms that have neither `dart:io` nor
/// `dart:js_interop`.
///
/// There is no such platform today (a Dart program is either on the VM or
/// compiled to JS/Wasm), so this exists to keep the *structure* honest: the
/// unified API still compiles here, and every operation fails with a documented
/// [AnyCodecException] instead of a link error or an unhandled
/// `UnsupportedError`.
library;

import '../model/capability.dart';
import '../model/codec.dart';
import '../model/config.dart';
import '../model/error.dart';
import 'codec_runtime.dart';

/// Creates the unsupported-platform runtime.
CodecRuntime createRuntime() => UnsupportedCodecRuntime();

/// A runtime that refuses every operation.
final class UnsupportedCodecRuntime implements CodecRuntime {
  @override
  int get workerCount => 0;

  @override
  int get passthroughCodecId => 0;

  @override
  Future<void> initialize({int workers = 0}) async => _fail('initialize');

  @override
  Future<void> shutdown() async {}

  @override
  Future<List<CodecCapability>> capabilities() async =>
      throw _unsupported('capabilities');

  @override
  Future<List<HardwareDevice>> probeHardware() async =>
      throw _unsupported('probeHardware');

  @override
  Future<String> capabilityReport() async =>
      throw _unsupported('capabilityReport');

  @override
  Future<String> platformReport() async => throw _unsupported('platformReport');

  @override
  Future<AnycodecEncoder> encoder(
    EncoderConfig config, {
    BackendPreference preference = BackendPreference.auto,
  }) async => throw _unsupported('encoder');

  @override
  Future<AnycodecDecoder> decoder(
    DecoderConfig config, {
    BackendPreference preference = BackendPreference.auto,
  }) async => throw _unsupported('decoder');

  @override
  Future<AnycodecEncoder> diagnosticEncoder(Codec codec) async =>
      throw _unsupported('diagnosticEncoder');

  @override
  Future<AnycodecDecoder> diagnosticDecoder(Codec codec) async =>
      throw _unsupported('diagnosticDecoder');

  AnyCodecException _unsupported(String operation) =>
      AnyCodecException.unsupported(
        'AnyCodec does not support this platform: it has neither dart:io (the '
        'native runtime) nor dart:js_interop (the WebCodecs runtime)',
        operation: operation,
      );

  void _fail(String operation) => throw _unsupported(operation);
}
