use criterion::{black_box, criterion_group, criterion_main, Criterion};

// ── Helpers: generate test NAL data ──

/// Build an AVCC-format NAL unit (4-byte length prefix + nal_type byte + payload).
fn avcc_nal(nal_type: u8, payload_size: usize) -> Vec<u8> {
    let mut nal: Vec<u8> = Vec::with_capacity(1 + payload_size);
    nal.push(nal_type);
    nal.extend(std::iter::repeat_n(0xAA, payload_size));
    let len = (nal.len() as u32).to_be_bytes();
    [len.as_slice(), &nal].concat()
}

/// Build a `Vec<Vec<u8>>` of NAL units as they come from `avcc_nal_units`.
fn nal_units(count: usize, nal_type: u8, payload_size: usize) -> Vec<(Vec<u8>, bool)> {
    let mut units = Vec::with_capacity(count);
    for i in 0..count {
        let data = avcc_nal(nal_type, payload_size);
        // Strip the 4-byte AVCC length prefix to get the raw NAL
        let raw = data[4..].to_vec();
        let is_last = i == count - 1;
        units.push((raw, is_last));
    }
    units
}

// ── Packetization benchmarks ──

fn bench_packetize_single_nal(c: &mut Criterion) {
    // Small NAL (≤ MTU) — no fragmentation needed
    let nal = nal_units(1, 0x41, 1000).pop().unwrap().0;

    c.bench_function("packetize/single_nal_1KB", |b| {
        b.iter(|| {
            let packets = mac_screen_cast::packetize_nal(black_box(nal.clone()), true, 1200);
            black_box(packets);
        });
    });
}

fn bench_packetize_small_fua(c: &mut Criterion) {
    // 2 KB NAL → ~2 FU-A fragments (max_chunk=1198)
    let nal = nal_units(1, 0x65, 2000).pop().unwrap().0;

    c.bench_function("packetize/fua_2KB_2fragments", |b| {
        b.iter(|| {
            let packets = mac_screen_cast::packetize_nal(black_box(nal.clone()), true, 1200);
            black_box(packets);
        });
    });
}

fn bench_packetize_large_fua(c: &mut Criterion) {
    // 50 KB NAL → ~42 FU-A fragments
    let nal = nal_units(1, 0x65, 50_000).pop().unwrap().0;

    c.bench_function("packetize/fua_50KB_42fragments", |b| {
        b.iter(|| {
            let packets = mac_screen_cast::packetize_nal(black_box(nal.clone()), true, 1200);
            black_box(packets);
        });
    });
}

fn bench_packetize_max_fua(c: &mut Criterion) {
    // 200 KB NAL → ~167 FU-A fragments (worst-case IDR slice)
    let nal = nal_units(1, 0x65, 200_000).pop().unwrap().0;

    c.bench_function("packetize/fua_200KB_167fragments", |b| {
        b.iter(|| {
            let packets = mac_screen_cast::packetize_nal(black_box(nal.clone()), true, 1200);
            black_box(packets);
        });
    });
}

fn bench_packetize_fua_nal_type_preservation(c: &mut Criterion) {
    // Verify overhead: 2 bytes FU-A header per fragment
    let nal = nal_units(1, 0x21, 5000).pop().unwrap().0;

    c.bench_function("packetize/fua_5KB_header_overhead", |b| {
        b.iter(|| {
            let packets = mac_screen_cast::packetize_nal(black_box(nal.clone()), true, 1200);
            black_box(packets);
        });
    });
}

// ── AVCC parsing benchmarks ──

fn bench_avcc_parse_single(c: &mut Criterion) {
    let data = avcc_nal(0x41, 1000);

    c.bench_function("avcc_parse/single_1KB", |b| {
        b.iter(|| {
            let units = mac_screen_cast::avcc_nal_units(black_box(&data));
            black_box(units);
        });
    });
}

fn bench_avcc_parse_keyframe(c: &mut Criterion) {
    // Simulate a keyframe: SPS + PPS + IDR slice
    let sps = avcc_nal(0x67, 30);   // SPS
    let pps = avcc_nal(0x68, 10);   // PPS
    let idr = avcc_nal(0x65, 5000); // IDR slice
    let data = [sps.as_slice(), pps.as_slice(), idr.as_slice()].concat();

    c.bench_function("avcc_parse/keyframe_SPS+PPS+IDR", |b| {
        b.iter(|| {
            let units = mac_screen_cast::avcc_nal_units(black_box(&data));
            black_box(units);
        });
    });
}

fn bench_avcc_parse_many_small(c: &mut Criterion) {
    // 50 small NALs (common for screen content with lots of small motion blocks)
    let mut data = Vec::with_capacity(50 * (4 + 100));
    for _ in 0..50 {
        data.extend_from_slice(&avcc_nal(0x41, 96));
    }

    c.bench_function("avcc_parse/50_small_NALs", |b| {
        b.iter(|| {
            let units = mac_screen_cast::avcc_nal_units(black_box(&data));
            black_box(units);
        });
    });
}

// ── Combined pipeline benchmark ──

fn bench_pipeline_keyframe(c: &mut Criterion) {
    // Simulate send_frame for a keyframe: parse AVCC → skip SPS/PPS → packetize NALs
    let sps = avcc_nal(0x67, 30);
    let pps = avcc_nal(0x68, 10);
    let idr = avcc_nal(0x65, 50_000);  // 50 KB IDR → ~42 fragments
    let data = [sps.as_slice(), pps.as_slice(), idr.as_slice()].concat();

    c.bench_function("pipeline/keyframe_parse+packetize", |b| {
        b.iter(|| {
            let frame_data = black_box(&data);
            let mut packets = Vec::with_capacity(64);

            // SPS (is_keyframe=true, skips NAL types 7,8 in the main loop)
            // In real send_frame, SPS/PPS are sent separately via packetize_nal
            packets.extend(mac_screen_cast::packetize_nal(
                black_box(vec![0x67, 0x01, 0x02, 0x03]),
                false,
                1200,
            ));
            packets.extend(mac_screen_cast::packetize_nal(
                black_box(vec![0x68, 0x01]),
                false,
                1200,
            ));

            // Parse AVCC data
            for (nal, is_last) in mac_screen_cast::avcc_nal_units(frame_data) {
                // Skip SPS/PPS on keyframes (NAL types 7, 8)
                if let Some(7 | 8) = nal.first().map(|b| b & 0x1f) {
                    continue;
                }
                packets.extend(mac_screen_cast::packetize_nal(nal, is_last, 1200));
            }

            black_box(packets);
        });
    });
}

fn bench_pipeline_delta_frame(c: &mut Criterion) {
    // Simulate send_frame for a P-frame: parse AVCC → packetize single large NAL
    let data = avcc_nal(0x41, 10_000);  // 10 KB P-frame NAL → ~9 fragments

    c.bench_function("pipeline/delta_frame_parse+packetize", |b| {
        b.iter(|| {
            let frame_data = black_box(&data);
            let mut packets = Vec::with_capacity(16);

            for (nal, is_last) in mac_screen_cast::avcc_nal_units(frame_data) {
                packets.extend(mac_screen_cast::packetize_nal(nal, is_last, 1200));
            }

            black_box(packets);
        });
    });
}

// ── Group and run ──

criterion_group!(
    benches,
    bench_packetize_single_nal,
    bench_packetize_small_fua,
    bench_packetize_large_fua,
    bench_packetize_max_fua,
    bench_packetize_fua_nal_type_preservation,
    bench_avcc_parse_single,
    bench_avcc_parse_keyframe,
    bench_avcc_parse_many_small,
    bench_pipeline_keyframe,
    bench_pipeline_delta_frame,
);
criterion_main!(benches);
