/// Platform-specific AnyCodec APIs.
///
/// The unified API in `package:anycodec/anycodec.dart` is identical everywhere.
/// This library is for the things that are *not*: the loaded native library and
/// its thread-local error channel on native, and WebCodecs capability probing on
/// the web. The two sets are chosen by conditional export, so `dart:ffi` never
/// leaks into a web build and `dart:js_interop` never leaks into a native one.
///
/// * **native (`io`)**: [NativeLibraryInfo], [LastNativeError],
///   [nativePassthroughCodecId], [nativeAssetId].
/// * **web**: [WebCodecs], [WebCodecsSupport], [WebCodecsConfigSupport],
///   [webCodecStringFor], [webPixelFormatFor].
/// * **all platforms**: the report types, re-exported here so a caller that only
///   needs [CodecPath] or [HardwareDevice] does not have to import both
///   libraries: [Backend], [BackendPreference], [Codec], [CodecCapability],
///   [CodecDirection], [CodecPath], [HardwareDevice], [HardwareDeviceCodec],
///   [Implementation].
library;

export 'src/model/capability.dart';
export 'src/model/codec.dart';
export 'src/model/config.dart';
export 'src/model/error.dart';
export 'src/model/format.dart';
export 'src/platform/platform_stub.dart'
    if (dart.library.io) 'src/platform/platform_io.dart'
    if (dart.library.js_interop) 'src/platform/platform_web.dart';
