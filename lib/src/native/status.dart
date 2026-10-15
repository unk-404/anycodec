/// The ABI status codes, and the thread-local last-error channel behind them.
///
/// The C ABI reports failures as a negative `i32` status and keeps the detail in
/// a per-thread slot. This library is the bridge: it turns the two into the
/// [AnyCodecException] type, which lives in the memory-safe model layer
/// (`src/model/error.dart`) so that the web runtime can throw it without ever
/// importing `dart:ffi`.
library;

import 'dart:ffi';
import 'dart:typed_data';

import 'package:ffi/ffi.dart' as pkg_ffi;

import '../bindings/anycodec_bindings_generated.dart' as bindings;
import '../model/error.dart';

export '../model/error.dart' show AnyCodecErrorKind, AnyCodecException;

/// The call succeeded.
const int statusOk = 0;

/// Used only by `*_receive` / `*_extradata`: an output object was produced.
const int statusGotOutput = 1;

/// Used only by `*_receive` / `*_extradata`: more input is required.
const int statusNeedMoreInput = 0;

/// A caller-supplied argument was malformed.
const int statusInvalidArgument = -1;

/// The codec or operation is not implemented by this build.
const int statusUnsupported = -2;

/// Implemented, but no backend is currently usable.
const int statusUnavailable = -3;

/// The platform API reported a failure.
const int statusBackend = -4;

/// The object is not in a state that permits the call.
const int statusState = -5;

/// Allocation failed.
const int statusOutOfMemory = -6;

/// A file or stream operation failed.
const int statusIo = -7;

/// An internal invariant was violated, or a Rust panic was caught.
const int statusInternal = -8;

/// The status code of this thread's last AnyCodec call.
///
/// `0` means success. Read it only on the isolate that made the call: the slot
/// is thread-local, and a worker isolate is the only place native code runs.
int lastNativeErrorStatus() => bindings.anycodec_last_error_status();

/// The message of this thread's last AnyCodec call, copied into a Dart string.
///
/// The native pointer is per-thread and only valid until the next AnyCodec call,
/// so the bytes are copied immediately. Returns an empty string when there is no
/// message.
String lastNativeErrorMessage() {
  final pointer = bindings.anycodec_last_error_message();
  if (pointer == nullptr) {
    return '';
  }
  return pointer.cast<pkg_ffi.Utf8>().toDartString();
}

/// Clears this thread's last error.
void clearNativeError() => bindings.anycodec_clear_error();

/// A snapshot of the thread-local last-error channel.
final class LastNativeError {
  /// Builds a snapshot.
  const LastNativeError({
    required this.status,
    required this.kind,
    required this.message,
  });

  /// The raw `ANYCODEC_STATUS_*` code (`0` when nothing failed).
  final int status;

  /// The classification of [status].
  final AnyCodecErrorKind kind;

  /// The message, or an empty string when there is none.
  final String message;

  /// Reads the channel.
  ///
  /// The message is read *before* the status so that the status call cannot
  /// invalidate the message pointer, even though neither of them may clear the
  /// slot.
  static LastNativeError read() {
    final message = lastNativeErrorMessage();
    final status = lastNativeErrorStatus();
    return LastNativeError(
      status: status,
      kind: AnyCodecErrorKind.fromStatus(status),
      message: message,
    );
  }

  /// Clears this thread's last error.
  static void clear() => clearNativeError();

  /// `true` when the last call succeeded.
  bool get isOk => status == 0;

  @override
  String toString() => 'LastNativeError($status, ${kind.token}, "$message")';
}

/// Turns a failed ABI status into an [AnyCodecException].
///
/// [status] must be negative (or at least not one of the success codes); the
/// message is read from the native thread-local slot and copied. This is the
/// single conversion point between the C ABI's status codes and Dart exceptions.
AnyCodecException exceptionForStatus(int status, String operation) {
  final error = LastNativeError.read();
  final kind = AnyCodecErrorKind.fromStatus(status);
  final message = error.message.isEmpty
      ? '$operation failed with status $status (${kind.token}) and no message'
      : error.message;
  return AnyCodecException(
    kind: kind,
    message: message,
    status: status,
    operation: operation,
  );
}

/// Builds a [Uint8List] copy of [length] bytes at [pointer].
///
/// Returns an empty list for a NULL pointer or a zero length, which is how the
/// ABI encodes "no payload". A copy, never a view: the native owner is released
/// as soon as the caller's `try`/`finally` unwinds.
Uint8List copyNativeBytes(Pointer<UnsignedChar> pointer, int length) {
  if (pointer == nullptr || length <= 0) {
    return Uint8List(0);
  }
  return Uint8List.fromList(pointer.cast<Uint8>().asTypedList(length));
}
