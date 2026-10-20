/// A small arena that guarantees every native allocation is released.
///
/// Every ABI call that needs scratch memory (configuration structs, plane
/// pointer arrays, output slots, C strings) runs inside
/// [AnyCodecArena.scope], which releases everything it handed out even when the
/// body throws. Native pointers therefore cannot leak on an error path, and the
/// call sites do not need `try`/`finally` spread through them.
///
/// ## Why the element size is passed explicitly
///
/// `dart:ffi`'s `sizeOf<T>()` (and `calloc<T>()`) reject a type *parameter*: the
/// type argument has to be a compile-time constant. A generic arena therefore
/// cannot compute a size for `T`, so [allocate] takes the byte count and call
/// sites spell out `sizeOf<ConcreteType>()` themselves.
library;

import 'dart:ffi';

import 'package:ffi/ffi.dart' as pkg_ffi;

/// A scope of native allocations that are all released together.
///
/// Not thread-safe and not reusable: create one per call, use
/// [AnyCodecArena.scope] so it is always disposed.
final class AnyCodecArena {
  /// Creates an empty arena.
  AnyCodecArena();

  final List<void Function()> _releases = <void Function()>[];
  bool _disposed = false;

  /// Runs [body] with a fresh arena and releases it afterwards.
  ///
  /// Releases happen in reverse allocation order, including when [body] throws;
  /// the original exception always propagates.
  static T scope<T>(T Function(AnyCodecArena arena) body) {
    final arena = AnyCodecArena();
    try {
      return body(arena);
    } finally {
      arena.dispose();
    }
  }

  /// Allocates [byteCount] zeroed bytes and schedules the free.
  ///
  /// Pass `sizeOf<ConcreteType>() * count`; the type argument only decides the
  /// pointer's Dart type. Throws [StateError] when the arena has already been
  /// disposed.
  Pointer<T> allocate<T extends NativeType>(int byteCount) {
    _checkUsable();
    final pointer = pkg_ffi.calloc.allocate<T>(byteCount);
    _releases.add(() => pkg_ffi.calloc.free(pointer));
    return pointer;
  }

  /// Allocates [length] zeroed bytes.
  ///
  /// A zero [length] yields `nullptr`, because allocating zero bytes is not
  /// portable and the ABI reads "NULL with length 0" as an empty buffer. The
  /// element type is `UnsignedChar` because that is what the generated bindings
  /// use for every `const unsigned char*` parameter; cast to `Uint8` to get a
  /// `Uint8List` view.
  Pointer<UnsignedChar> allocateBytes(int length) {
    _checkUsable();
    if (length <= 0) {
      return nullptr.cast<UnsignedChar>();
    }
    return allocate<UnsignedChar>(length);
  }

  /// Allocates a slot for one output pointer, initialised to `nullptr`.
  Pointer<Pointer<T>> allocateSlot<T extends NativeType>() {
    _checkUsable();
    final slot = allocate<Pointer<T>>(sizeOf<Pointer<Void>>());
    slot.value = nullptr;
    return slot;
  }

  /// Allocates a NUL-terminated copy of [text].
  Pointer<pkg_ffi.Utf8> allocateString(String text) {
    _checkUsable();
    final pointer = text.toNativeUtf8(allocator: pkg_ffi.calloc);
    _releases.add(() => pkg_ffi.calloc.free(pointer));
    return pointer;
  }

  /// Allocates a `const char* const[]` holding copies of [values].
  ///
  /// The array itself and every string live in this arena; a zero-length [values]
  /// yields `nullptr`, which is what the ABI expects when `option_count` is `0`.
  Pointer<Pointer<Char>> allocateStrings(List<String> values) {
    _checkUsable();
    if (values.isEmpty) {
      return nullptr.cast<Pointer<Char>>();
    }
    final array = allocate<Pointer<Char>>(
      sizeOf<Pointer<Void>>() * values.length,
    );
    for (var index = 0; index < values.length; index++) {
      array[index] = allocateString(values[index]).cast<Char>();
    }
    return array;
  }

  /// Releases everything this arena allocated.
  ///
  /// Idempotent; further allocation attempts throw [StateError]. A release
  /// callback that throws does not stop the others from running.
  void dispose() {
    if (_disposed) {
      return;
    }
    _disposed = true;
    Object? firstError;
    StackTrace? firstStack;
    for (var index = _releases.length - 1; index >= 0; index--) {
      try {
        _releases[index]();
      } catch (error, stack) {
        firstError ??= error;
        firstStack ??= stack;
      }
    }
    _releases.clear();
    if (firstError != null) {
      Error.throwWithStackTrace(firstError, firstStack!);
    }
  }

  void _checkUsable() {
    if (_disposed) {
      throw StateError('this AnyCodecArena has already been disposed');
    }
  }
}
