# anycodec example

A small Flutter app that demonstrates the Dart layer. Run it from this directory:

```sh
flutter run            # native (Android, iOS, macOS, Windows, Linux)
flutter run -d chrome  # web (WebCodecs)
```

Four tabs, one per claim the package makes:

1. **Capabilities** — the report for all twelve codecs in both directions, with
   every registered backend, whether it is usable, and why not. The hardware and
   software-fallback badges come straight from `CodecCapability`.
2. **Round trip** — encodes and decodes five synthetic I420 frames through the
   diagnostic codec (which needs no platform support), checks the geometry,
   timestamps and every plane byte, and prints the `CodecPath` the sessions
   actually reported.
3. **Responsiveness** — a 5 ms timer keeps counting while 300 frames are encoded.
   Every native call runs on a worker isolate, so the counter must not freeze.
4. **Platform APIs** — the platform-specific extras. On native: the loaded
   library's ABI revision, version, native asset, best-effort path, the
   thread-local last error and the worker count. On web: which codecs WebCodecs
   accepts, and the honest note that `package:web` exposes WebCodecs video only.

The app is deliberately not a benchmark: it exists to show that the plumbing is
wired up, and to make the software-fallback reporting visible.
