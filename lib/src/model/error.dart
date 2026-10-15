/// The exception type every failing AnyCodec operation throws.
///
/// This lives in the memory-safe model layer, **not** in `src/native/`: the web
/// runtime has to be able to throw it, and `src/native/**` imports `dart:ffi`,
/// which does not exist in a browser. The mapping from an ABI status code to one
/// of these (and the thread-local last-error reads) stays in
/// `src/native/status.dart`, which is the only place that can see the ABI.
library;

/// The classification attached to an [AnyCodecException].
///
/// The numeric values are the `ANYCODEC_STATUS_*` constants of the C ABI; they
/// are stable and never renumbered. [token] matches the Rust `ErrorKind::as_str`
/// vocabulary, so a Dart log line and a Rust log line can be compared directly.
enum AnyCodecErrorKind {
  /// A caller-supplied argument was malformed (NULL, out of range, ...).
  invalidArgument('invalid_argument', -1),

  /// The codec or operation is not implemented by this build.
  unsupported('unsupported', -2),

  /// Implemented, but no backend is currently usable.
  unavailable('unavailable', -3),

  /// The platform API reported a failure.
  backend('backend', -4),

  /// The object is not in a state that permits the call.
  state('state', -5),

  /// Allocation failed.
  outOfMemory('out_of_memory', -6),

  /// A file or stream operation failed.
  io('io', -7),

  /// An invariant was violated, or a Rust panic was caught. This is a bug.
  internal('internal', -8),

  /// A status this build does not recognise.
  unknown('unknown', 0);

  const AnyCodecErrorKind(this.token, this.status);

  /// Stable lower-case token.
  final String token;

  /// The `ANYCODEC_STATUS_*` code.
  final int status;

  /// Maps an ABI status onto a kind. Unknown values become
  /// [AnyCodecErrorKind.unknown] rather than throwing.
  static AnyCodecErrorKind fromStatus(int status) {
    for (final kind in values) {
      if (kind.status == status && kind != AnyCodecErrorKind.unknown) {
        return kind;
      }
    }
    return AnyCodecErrorKind.unknown;
  }

  /// `true` for failures that mean "AnyCodec has a bug", as opposed to "this
  /// machine cannot do this".
  bool get isBug => this == AnyCodecErrorKind.internal;

  /// `true` when the failure is the caller's fault rather than the machine's.
  bool get isCallerError => this == AnyCodecErrorKind.invalidArgument;
}

/// The exception every failing AnyCodec operation throws.
///
/// Native failures carry the ABI status and the message the Rust side recorded;
/// failures detected in Dart (a frame that does not match its codec, a closed
/// session) carry the same kind vocabulary so callers only have to learn one.
final class AnyCodecException implements Exception {
  /// Builds an exception.
  const AnyCodecException({
    required this.kind,
    required this.message,
    this.status = 0,
    this.operation,
  });

  /// A caller-supplied argument was malformed.
  factory AnyCodecException.invalidArgument(
    String message, {
    String? operation,
  }) => AnyCodecException(
    kind: AnyCodecErrorKind.invalidArgument,
    message: message,
    status: AnyCodecErrorKind.invalidArgument.status,
    operation: operation,
  );

  /// The codec or operation is not implemented by this build.
  factory AnyCodecException.unsupported(String message, {String? operation}) =>
      AnyCodecException(
        kind: AnyCodecErrorKind.unsupported,
        message: message,
        status: AnyCodecErrorKind.unsupported.status,
        operation: operation,
      );

  /// Implemented, but nothing on this machine can run it.
  factory AnyCodecException.unavailable(String message, {String? operation}) =>
      AnyCodecException(
        kind: AnyCodecErrorKind.unavailable,
        message: message,
        status: AnyCodecErrorKind.unavailable.status,
        operation: operation,
      );

  /// The object is not in a state that permits the call.
  factory AnyCodecException.state(String message, {String? operation}) =>
      AnyCodecException(
        kind: AnyCodecErrorKind.state,
        message: message,
        status: AnyCodecErrorKind.state.status,
        operation: operation,
      );

  /// An invariant was violated, including "a worker isolate died".
  factory AnyCodecException.internal(String message, {String? operation}) =>
      AnyCodecException(
        kind: AnyCodecErrorKind.internal,
        message: message,
        status: AnyCodecErrorKind.internal.status,
        operation: operation,
      );

  /// The failure classification.
  final AnyCodecErrorKind kind;

  /// Human readable detail, as produced by the Rust side (or by the Dart layer
  /// for its own checks).
  final String message;

  /// The raw `ANYCODEC_STATUS_*` code, or the Dart layer's own status.
  final int status;

  /// The ABI entry point (or the Dart operation) that failed, when known.
  final String? operation;

  /// `true` when this failure means "AnyCodec has a bug".
  bool get isBug => kind.isBug;

  @override
  String toString() {
    final buffer = StringBuffer('AnyCodecException(')..write(kind.token);
    if (operation != null) {
      buffer
        ..write(' from ')
        ..write(operation);
    }
    buffer
      ..write('): ')
      ..write(message.isEmpty ? '(no message)' : message);
    return buffer.toString();
  }
}
