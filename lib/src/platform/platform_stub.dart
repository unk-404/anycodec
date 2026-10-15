/// An empty stand-in for the platform extras.
///
/// Selected only on a platform that has neither `dart:io` nor
/// `dart:js_interop`, where `platform_io.dart` and `platform_web.dart` cannot
/// compile. There is no such platform today; the file exists so that
/// `anycodec_platform.dart` always resolves, and so that a future one gets a
/// compile-time error to fix rather than a link-time one.
library;
