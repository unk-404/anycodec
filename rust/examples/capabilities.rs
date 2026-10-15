//! Prints the codec capability report for the machine this runs on.
//!
//! ```text
//! cargo run --example capabilities
//! ```
//!
//! This is the human-readable form of what
//! `anycodec_capabilities()` / `AnyCodec.capabilities()` return, including the
//! *reason* a backend is unavailable. It is the fastest way to answer "why did my
//! encode fall back to software?".

use anycodec::engine::{BackendKind, CodecKind};

fn main() {
    println!("anycodec {} (ABI {})", anycodec::VERSION, anycodec::ANYCODEC_ABI_VERSION);
    println!();
    println!("{:<8} {:<7} {:<10} {}", "codec", "dir", "backend", "availability");
    println!("{}", "-".repeat(96));

    let reports = anycodec::engine::registry().capabilities();
    for report in &reports {
        for (kind, entries) in
            [(CodecKind::Encoder, &report.encoders), (CodecKind::Decoder, &report.decoders)]
        {
            for entry in entries.iter() {
                let availability = if entry.available {
                    "ready".to_string()
                } else {
                    entry.note.clone()
                };
                println!(
                    "{:<8} {:<7} {:<10} {}{}",
                    report.codec.name(),
                    kind.name(),
                    entry.backend.name(),
                    if entry.hardware { "[hardware-capable] " } else { "" },
                    availability,
                );
            }
        }
    }

    println!();
    println!("software fallbacks");
    println!("{}", "-".repeat(96));
    if let Some(info) = probe_ffmpeg() {
        println!("  libavcodec: {info}");
    }
    for report in &reports {
        let software: Vec<BackendKind> = report
            .encoders
            .iter()
            .chain(report.decoders.iter())
            .filter(|entry| entry.available && entry.backend.is_software())
            .map(|entry| entry.backend)
            .collect();
        if software.is_empty() {
            println!("  {:<8} none available on this machine", report.codec.name());
        }
    }

    println!();
    println!("hardware engines");
    println!("{}", "-".repeat(96));
    let devices = anycodec::engine::device::devices();
    if devices.is_empty() {
        println!("  none reported");
    }
    for device in &devices {
        println!(
            "  {} [{}] {} codecs",
            device.name,
            device.api_version,
            device.codecs.len()
        );
    }
}

/// Reads the FFmpeg tier's probe without pulling in the whole tier.
fn probe_ffmpeg() -> Option<String> {
    for report in anycodec::engine::registry().capabilities() {
        for entry in report.encoders.iter().chain(report.decoders.iter()) {
            if entry.backend == BackendKind::Ffmpeg {
                return Some(entry.note.clone());
            }
        }
    }
    None
}
