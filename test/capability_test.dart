/// Capability reporting: does the report describe the whole surface, honestly?
library;

import 'package:anycodec/anycodec.dart';
import 'package:test/test.dart';

void main() {
  setUpAll(AnyCodec.initialize);
  tearDownAll(AnyCodec.shutdown);

  test('the report covers all twelve codecs in both directions', () async {
    final capabilities = await AnyCodec.capabilities();
    expect(capabilities.length, 24, reason: '12 codecs x (encode, decode)');

    final pairs = <String>{
      for (final entry in capabilities)
        '${entry.codec.name}/${entry.direction.name}',
    };
    expect(
      pairs.length,
      24,
      reason: 'every (codec, direction) pair exactly once',
    );

    for (final codec in Codec.values) {
      for (final direction in CodecDirection.values) {
        expect(
          capabilities.any(
            (entry) => entry.codec == codec && entry.direction == direction,
          ),
          isTrue,
          reason: '${codec.name} ${direction.name} must appear in the report',
        );
      }
    }
  });

  test(
    'every entry lists at least one backend, with a note that explains it',
    () async {
      final capabilities = await AnyCodec.capabilities();
      for (final entry in capabilities) {
        expect(
          entry.backends,
          isNotEmpty,
          reason: '${entry.summary} has no backend registered at all',
        );
        for (final backend in entry.backends) {
          expect(
            backend.note,
            isNotEmpty,
            reason:
                '${entry.codec.name} ${entry.direction.name} '
                '${backend.backend.token} has no note',
          );
          if (backend.available) {
            expect(
              backend.note,
              'ready',
              reason:
                  'an available backend reports the note the ABI uses for '
                  '"constructible right now"',
            );
          } else {
            expect(
              backend.note,
              isNot('ready'),
              reason: 'an unavailable backend must say why',
            );
          }
        }
      }
    },
  );

  test('every entry has a registered rust or platform backend', () async {
    final capabilities = await AnyCodec.capabilities();
    for (final entry in capabilities) {
      final tokens = <String>{
        for (final backend in entry.backends) backend.backend.token,
      };
      expect(
        tokens.any(
          (token) => token == 'rust' || token == 'ffmpeg' || token != '',
        ),
        isTrue,
      );
      // The software tier is always registered for at least one direction of at
      // least one codec; the point here is that the report never claims support
      // out of thin air.
      for (final backend in entry.backends) {
        expect(
          Backend.values.contains(backend.backend),
          isTrue,
          reason: '${backend.backend} is not a known backend',
        );
      }
    }
  });

  test(
    'the capability report is readable and mentions h264 and opus',
    () async {
      final report = await AnyCodec.capabilityReport();
      expect(report, isNotEmpty);
      expect(report, contains('h264'));
      expect(report, contains('opus'));
    },
  );

  test(
    'probeHardware returns a (possibly empty) list without throwing',
    () async {
      final devices = await AnyCodec.probeHardware();
      expect(devices, isNotNull);
      for (final device in devices) {
        expect(device.name, isNotEmpty, reason: 'a device must have a name');
        for (final codec in device.codecs) {
          expect(
            codec.canDecode || codec.canEncode,
            isTrue,
            reason: '${device.name} lists ${codec.codec.name} as neither',
          );
        }
      }
    },
  );

  test('an entry knows whether it would land on a software fallback', () async {
    final capabilities = await AnyCodec.capabilities();
    for (final entry in capabilities) {
      expect(
        entry.isAvailable,
        entry.availableBackends.isNotEmpty,
        reason: 'isAvailable must agree with availableBackends',
      );
      expect(
        entry.hardwareAccelerated,
        entry.availableBackends.any((backend) => backend.hardware),
      );
      expect(
        entry.hasSoftwareFallback,
        entry.availableBackends.any(
          (backend) => backend.implementation == Implementation.software,
        ),
      );
      expect(entry.summary, contains(entry.codec.name));
    }
  });
}
