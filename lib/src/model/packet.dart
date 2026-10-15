/// Compressed packets and their flags.
library;

import 'dart:typed_data';

import 'codec.dart';

/// The `ANYCODEC_PACKET_FLAG_*` bitmask, as a value type.
///
/// Prefer the named constants ([PacketFlags.keyframe], ...) and [contains] over
/// poking at [bits] directly; [bits] exists so a mask can be handed to the ABI
/// and so callers can keep bits this build does not know about.
final class PacketFlags {
  /// Wraps a raw ABI bitmask.
  const PacketFlags(this.bits);

  /// The raw `ANYCODEC_PACKET_FLAG_*` bitmask.
  final int bits;

  /// No flags set.
  static const PacketFlags none = PacketFlags(0);

  /// The packet can be decoded without any earlier packet.
  static const PacketFlags keyframe = PacketFlags(1 << 0);

  /// The packet carries codec configuration rather than a picture.
  static const PacketFlags header = PacketFlags(1 << 1);

  /// The packet marks the end of the stream.
  static const PacketFlags eos = PacketFlags(1 << 2);

  /// The packet was produced by a hardware accelerated pipeline.
  static const PacketFlags hardware = PacketFlags(1 << 3);

  /// The packet is known to be corrupt or only partially decoded.
  static const PacketFlags corrupt = PacketFlags(1 << 4);

  /// `true` when *every* bit of [other] is set in this mask.
  bool contains(PacketFlags other) =>
      other.bits == 0 || (bits & other.bits) == other.bits;

  /// `true` when the packet is a keyframe.
  bool get isKeyframe => contains(keyframe);

  /// `true` when the packet carries codec configuration.
  bool get isHeader => contains(header);

  /// `true` when the packet marks the end of the stream.
  bool get isEndOfStream => contains(eos);

  /// `true` when the packet came out of a hardware pipeline.
  bool get isHardware => contains(hardware);

  /// `true` when the packet is known to be damaged.
  bool get isCorrupt => contains(corrupt);

  /// This mask with the bits of [other] added.
  PacketFlags operator |(PacketFlags other) => PacketFlags(bits | other.bits);

  /// This mask with the bits of [other] removed.
  PacketFlags operator &(PacketFlags other) => PacketFlags(bits & other.bits);

  /// The names of the known bits that are set, in ABI order.
  List<String> get names => <String>[
    if (isKeyframe) 'keyframe',
    if (isHeader) 'header',
    if (isEndOfStream) 'eos',
    if (isHardware) 'hardware',
    if (isCorrupt) 'corrupt',
  ];

  @override
  bool operator ==(Object other) => other is PacketFlags && other.bits == bits;

  @override
  int get hashCode => bits.hashCode;

  @override
  String toString() =>
      names.isEmpty ? 'PacketFlags.none' : 'PacketFlags(${names.join('|')})';
}

/// One compressed unit of a bitstream.
///
/// ## Timebase
///
/// [pts], [dts] and [duration] are **microseconds**, always, for every codec and
/// every platform. The C ABI itself is timebase-agnostic ("in the stream
/// timebase"), so the Dart layer picks one and documents it here rather than
/// letting each backend invent its own: microseconds are what WebCodecs uses,
/// what `Duration` converts to without loss, and a single unit keeps an encode
/// and a decode of the same clip comparable. A backend that works in a
/// different timebase (a 90 kHz MPEG clock, say) is expected to convert on the
/// way out; anything it reports through this class is microseconds.
///
/// A value of `0` means "unknown" for [duration] and is a legal timestamp for
/// [pts].
final class Packet {
  /// Builds a packet over [data].
  ///
  /// [data] is borrowed, not copied: the caller keeps ownership and must not
  /// mutate it while a session is using the packet. Passing an empty list is
  /// allowed (the ABI accepts a zero-length payload).
  const Packet({
    required this.codec,
    required this.data,
    this.pts = 0,
    this.dts = 0,
    this.duration = 0,
    this.flags = PacketFlags.none,
  });

  /// The codec this packet belongs to.
  final Codec codec;

  /// The compressed bytes.
  final Uint8List data;

  /// Presentation timestamp in microseconds.
  final int pts;

  /// Decode timestamp in microseconds. Equals [pts] when the codec has no
  /// reordering.
  final int dts;

  /// Duration in microseconds, or `0` when unknown.
  final int duration;

  /// The `ANYCODEC_PACKET_FLAG_*` bits.
  final PacketFlags flags;

  /// Number of payload bytes.
  int get length => data.length;

  /// `true` for a zero-length payload.
  bool get isEmpty => data.isEmpty;

  /// The same packet with different timestamps.
  Packet withTimestamps({int? pts, int? dts, int? duration}) => Packet(
    codec: codec,
    data: data,
    pts: pts ?? this.pts,
    dts: dts ?? this.dts,
    duration: duration ?? this.duration,
    flags: flags,
  );

  @override
  String toString() =>
      'Packet(${codec.name}, $length bytes, pts=$pts us, dts=$dts us, '
      'duration=$duration us, flags=$flags)';
}
