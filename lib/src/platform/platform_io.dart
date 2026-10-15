/// Native-platform extras: what library is loaded, and what it last said.
///
/// This is the file `package:anycodec/anycodec_platform.dart` exposes on a
/// platform with `dart:io`. It is the only public surface that talks to the ABI
/// directly, and it does so for two things the worker pool cannot usefully own:
/// identifying the loaded library, and reading the thread-local error channel of
/// the *calling* isolate.
library;

import 'dart:io';

import '../native/native_bindings.dart';

export '../native/status.dart' show LastNativeError;

/// What is known about the loaded native library.
///
/// ## About [libraryPath]
///
/// Dart's native-asset toolchain resolves `@ffi.Native` symbols through an asset
/// mapping and does **not** expose the on-disk path of the library it mapped: a
/// `DynamicLibrary` handle is never handed to Dart code. [libraryPath] is
/// therefore best effort — it reports `Platform.environment['ANYCODEC_LIBRARY_PATH']`
/// when an embedding application sets it, and is `null` otherwise. [assetId] is
/// always available and is what the toolchain resolves; on a Flutter or Dart
/// build it corresponds to a `.dll`/`.so`/`.dylib` under
/// `.dart_tool/hooks_runner/`.
final class NativeLibraryInfo {
  /// Builds a library description.
  const NativeLibraryInfo({
    required this.abiVersion,
    required this.expectedAbiVersion,
    required this.versionString,
    required this.assetId,
    required this.libraryPath,
  });

  /// The ABI revision the loaded library reports.
  final int abiVersion;

  /// The ABI revision these Dart bindings were generated for.
  final int expectedAbiVersion;

  /// The library's semantic version, e.g. `"0.0.1 (abi 1)"`.
  final String versionString;

  /// The native-asset id `@ffi.Native` resolves through.
  final String assetId;

  /// The on-disk path of the library, when the embedder advertises it.
  final String? libraryPath;

  /// `true` when the library and the bindings agree on the ABI revision.
  bool get abiMatches => abiVersion == expectedAbiVersion;

  /// Reads the library's identity.
  ///
  /// Throws `AnyCodecException` when the ABI cannot be read at all (which can
  /// only happen if the native asset failed to load).
  static NativeLibraryInfo read() => NativeLibraryInfo(
    abiVersion: NativeBindings.abiVersion(),
    expectedAbiVersion: NativeBindings.expectedAbiVersion,
    versionString: NativeBindings.version().toString(),
    assetId: nativeAssetId,
    libraryPath: Platform.environment['ANYCODEC_LIBRARY_PATH'],
  );

  @override
  String toString() =>
      'NativeLibraryInfo(abi $abiVersion, $versionString, '
      'path=${libraryPath ?? '(not exposed)'})';
}

/// The native-asset id of the generated bindings.
///
/// Kept in sync with `ffigen.yaml` and `hook/build.dart` by construction: it is
/// the `@ffi.DefaultAsset` annotation on
/// `lib/src/bindings/anycodec_bindings_generated.dart`.
const String nativeAssetId =
    'package:anycodec/src/bindings/anycodec_bindings_generated.dart';

/// The codec slot the diagnostic codec advertises through the registry-facing
/// API.
///
/// This is not a production codec id: the diagnostic codec has its own entry
/// points precisely so that it can never be selected for a real codec.
int nativePassthroughCodecId() => NativeBindings.passthroughCodecId();
