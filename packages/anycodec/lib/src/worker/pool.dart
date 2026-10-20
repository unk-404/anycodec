/// The worker isolate pool.
///
/// A pool is a fixed set of long-lived isolates, each of which owns native
/// handles for the whole session. Sessions are bound to a worker **round-robin
/// at creation time**, so several sessions genuinely run in parallel on
/// different threads, and every native call for one session happens on exactly
/// one thread (which is what the C ABI requires).
///
/// The main isolate only ever sends messages: it never calls a native function,
/// not even to probe for hardware. A worker that dies — a Rust panic, a
/// `SIGSEGV`, an explicit kill — fails every request that was in flight with an
/// [AnyCodecException]; nothing can hang waiting for a reply that will never
/// come.
library;

import 'dart:async';
import 'dart:io';
import 'dart:isolate';

import '../native/status.dart';
import 'protocol.dart';
import 'worker_main.dart';

/// A pool of worker isolates.
///
/// Not thread-safe in the Dart sense: like everything else in this package it
/// is meant to be driven from one isolate (the UI isolate).
final class WorkerPool {
  /// Creates an uninitialised pool.
  WorkerPool();

  List<WorkerHandle> _handles = const <WorkerHandle>[];
  int _next = 0;
  bool _initialized = false;

  /// `true` once [initialize] has completed.
  bool get isInitialized => _initialized;

  /// Number of workers, or `0` before initialisation.
  int get workerCount => _handles.length;

  /// The worker indices, in order.
  List<int> get workerIds => <int>[for (final handle in _handles) handle.id];

  /// Starts the pool, or reconfigures it.
  ///
  /// `workers == 0` (the default) means `max(1, numberOfProcessors - 1)`.
  /// Calling this again with the count already in use, or with `0`, does
  /// nothing. Throws [AnyCodecException] with `kind == state` when a different
  /// count is requested; the caller (the runtime) shuts the pool down first,
  /// and only when no session is open.
  Future<void> initialize({int workers = 0}) async {
    if (_initialized) {
      if (workers == 0 || workers == _handles.length) {
        return;
      }
      throw AnyCodecException.state(
        'the worker pool already has ${_handles.length} workers; call '
        'AnyCodec.shutdown() before changing the count',
      );
    }
    final count = workers > 0 ? workers : _defaultWorkerCount();
    final handles = <WorkerHandle>[
      for (var index = 0; index < count; index++) WorkerHandle(index),
    ];
    try {
      await Future.wait(<Future<void>>[
        for (final handle in handles) handle.ready,
      ]);
    } catch (_) {
      for (final handle in handles) {
        handle.abandon();
      }
      rethrow;
    }
    _handles = handles;
    _next = 0;
    _initialized = true;
  }

  /// Initialises the pool if it is not running yet, using the default size.
  ///
  /// Concurrent calls share one initialisation.
  Future<void> ensureInitialized() async {
    if (_initialized) {
      return;
    }
    await initialize();
  }

  /// Picks the next worker, round-robin.
  ///
  /// Synchronous on purpose: two sessions created "at the same time" must land
  /// on different workers, which they do because the counter advances before
  /// any `await`.
  WorkerHandle nextWorker() {
    if (!_initialized || _handles.isEmpty) {
      throw AnyCodecException.state(
        'the worker pool is not running; call AnyCodec.initialize() first',
      );
    }
    final handle = _handles[_next % _handles.length];
    _next = (_next + 1) % _handles.length;
    return handle;
  }

  /// Stops every worker. Idempotent.
  Future<void> shutdown() async {
    final handles = _handles;
    _handles = const <WorkerHandle>[];
    _initialized = false;
    _next = 0;
    await Future.wait(<Future<void>>[
      for (final handle in handles) handle.dispose(),
    ]);
  }

  static int _defaultWorkerCount() {
    final processors = Platform.numberOfProcessors;
    return processors > 1 ? processors - 1 : 1;
  }
}

/// One worker isolate.
///
/// Owns the request/response bookkeeping for that isolate. Every request gets an
/// id; the response with that id completes the matching future. If the isolate
/// dies, [isAlive] goes `false` and every pending request fails immediately.
final class WorkerHandle {
  /// Spawns worker [id].
  WorkerHandle(this.id) : _port = ReceivePort() {
    _port.listen(_onMessage);
    Isolate.spawn(
      anycodecWorkerMain,
      WorkerBoot(workerId: id, replyTo: _port.sendPort),
      onError: _port.sendPort,
      onExit: _port.sendPort,
      errorsAreFatal: true,
      debugName: 'anycodec-worker-$id',
    ).then(
      (isolate) => _isolate = isolate,
      onError: (Object error, StackTrace stack) {
        _fail(
          AnyCodecException.internal('worker $id could not be spawned: $error'),
        );
      },
    );
  }

  /// This worker's index in the pool.
  final int id;

  final ReceivePort _port;
  final Map<int, Completer<Object?>> _pending = <int, Completer<Object?>>{};
  final Completer<void> _ready = Completer<void>();
  Isolate? _isolate;
  SendPort? _requests;
  int _nextRequestId = 0;
  bool _alive = true;
  bool _disposed = false;

  /// Completes once the worker has verified the ABI and is accepting requests.
  ///
  /// Fails with an [AnyCodecException] carrying the isolate's error when the
  /// worker died during start-up (for example on an ABI mismatch).
  Future<void> get ready => _ready.future;

  /// `true` while this worker can still execute requests.
  bool get isAlive => _alive && !_disposed;

  /// Sends one request and waits for its response.
  ///
  /// Throws [AnyCodecException] when the worker is gone (before or during the
  /// call) or when the request itself failed on the far side.
  Future<Object?> send(WorkerOp op, [Object? payload]) {
    if (!isAlive) {
      return Future<Object?>.error(
        AnyCodecException.internal('worker $id is not running'),
      );
    }
    return _dispatch(op, payload);
  }

  Future<Object?> _dispatch(WorkerOp op, Object? payload) {
    final requests = _requests;
    if (requests == null) {
      return Future<Object?>.error(
        AnyCodecException.internal('worker $id is still starting up'),
      );
    }
    final requestId = _nextRequestId++;
    final completer = Completer<Object?>();
    _pending[requestId] = completer;
    requests.send(WorkerRequest(id: requestId, op: op, payload: payload));
    return completer.future;
  }

  /// Stops this worker: closes its sessions, then kills the isolate.
  ///
  /// Never throws. Pending requests fail with an [AnyCodecException].
  Future<void> dispose() async {
    if (_disposed) {
      return;
    }
    _disposed = true;
    if (_alive && _requests != null) {
      try {
        await _dispatch(
          WorkerOp.dispose,
          null,
        ).timeout(const Duration(seconds: 5));
      } catch (_) {
        // The worker was already gone, or did not answer in time; killing the
        // isolate below is what actually matters, and it does not leak because
        // the sessions live in that isolate.
      }
    }
    _fail(AnyCodecException.internal('worker $id was shut down'));
    _close();
  }

  /// Shuts the isolate down without waiting for a reply.
  ///
  /// Used when initialisation of the pool failed part-way through; it must not
  /// be able to hang.
  void abandon() {
    _fail(
      AnyCodecException.internal('worker $id was abandoned during start-up'),
    );
    _close();
  }

  void _close() {
    _alive = false;
    if (!_ready.isCompleted) {
      _ready.complete();
    }
    _port.close();
    _isolate?.kill(priority: Isolate.immediate);
  }

  void _onMessage(Object? message) {
    if (message is WorkerHello) {
      if (message.workerId == id) {
        _requests = message.requestPort;
        if (!_ready.isCompleted) {
          _ready.complete();
        }
      }
      return;
    }
    if (message is WorkerResponse) {
      final completer = _pending.remove(message.id);
      if (completer == null || completer.isCompleted) {
        return;
      }
      final failure = message.failure;
      if (failure != null) {
        completer.completeError(failure, StackTrace.current);
      } else {
        completer.complete(message.value);
      }
      return;
    }
    if (message is List) {
      // An uncaught error in the isolate: [errorString, stackTraceString].
      final detail = message.isEmpty ? 'unknown error' : '${message.first}';
      _fail(AnyCodecException.internal('worker $id crashed: $detail'));
      return;
    }
    if (message == null) {
      // The isolate exited.
      _fail(
        AnyCodecException.internal(
          'worker $id exited${_disposed ? '' : ' unexpectedly'}',
        ),
      );
    }
  }

  void _fail(AnyCodecException error) {
    _alive = false;
    final pending = _pending.values.toList(growable: false);
    _pending.clear();
    for (final completer in pending) {
      if (!completer.isCompleted) {
        completer.completeError(error, StackTrace.current);
      }
    }
    if (!_ready.isCompleted) {
      _ready.completeError(error, StackTrace.current);
    }
  }

  @override
  String toString() => 'WorkerHandle($id, ${isAlive ? 'alive' : 'dead'})';
}
