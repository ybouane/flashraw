//! Micro-benchmarks of hot kernels: bit pumps, Huffman/lossless JPEG decode, packed
//! unpack (scalar reference vs SIMD), and the develop pass. Synthetic inputs keep the

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use flashraw::low_level::codecs::{ljpeg, packed};
use flashraw::low_level::core::bits::{BitPumpLsb, BitPumpMsb, BitRead};
use flashraw::low_level::core::{Limits, PackOrder, RawImage};
use std::hint::black_box;

fn pseudo_bytes(n: usize) -> Vec<u8> {
    let mut x: u64 = 0x9E3779B97F4A7C15;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn bit_pumps(c: &mut Criterion) {
    let data = pseudo_bytes(8 << 20);
    let mut g = c.benchmark_group("bit_pumps");
    g.throughput(Throughput::Bytes(data.len() as u64));
    g.bench_function("msb_12bit", |b| {
        b.iter(|| {
            let mut p = BitPumpMsb::new(&data);
            let mut s = 0u32;
            for _ in 0..data.len() * 8 / 12 {
                s = s.wrapping_add(p.get(12));
            }
            black_box(s)
        })
    });
    g.bench_function("lsb_14bit", |b| {
        b.iter(|| {
            let mut p = BitPumpLsb::new(&data);
            let mut s = 0u32;
            for _ in 0..data.len() * 8 / 14 {
                s = s.wrapping_add(p.get(14));
            }
            black_box(s)
        })
    });
    g.finish();
}

fn unpack(c: &mut Criterion) {
    let (w, h) = (6000usize, 400usize);
    let mut g = c.benchmark_group("unpack");
    g.throughput(Throughput::Elements((w * h) as u64));
    for bits in [12u32, 14, 16] {
        for order in [PackOrder::MsbBytes, PackOrder::LsbBytes] {
            let src = pseudo_bytes(w * h * bits as usize / 8 + 16);
            let mut dst = vec![0u16; w * h];
            let layout = packed::PackedLayout::tight(w, h, bits, order);
            for (name, simd, scalar) in [("scalar-ref", false, true), ("fast", false, false), ("simd", true, false)] {
                g.bench_with_input(BenchmarkId::new(format!("{bits}bit-{order:?}"), name), &(), |b, _| {
                    b.iter(|| {
                        if scalar {
                            let rb = layout.row_bytes();
                            for y in 0..h {
                                packed::unpack_row_scalar(&src[y * rb..], &mut dst[y * w..(y + 1) * w], bits, order);
                            }
                        } else {
                            packed::unpack(&src, layout, &mut dst, w, false, simd, false).unwrap();
                        }
                        black_box(&dst);
                    })
                });
            }
        }
    }
    g.finish();
}

fn lossless_jpeg(c: &mut Criterion) {
    let (w, h) = (2000usize, 1000usize);
    let mut x = 1000i32;
    let samples: Vec<u16> = (0..w * h)
        .map(|i| {
            x = (x + ((i as i32 * 7919) % 61) - 30).clamp(0, 16383);
            x as u16
        })
        .collect();
    let jpg = ljpeg::encoder::encode(w / 2, h, 2, 14, 1, &samples);
    let hdr = ljpeg::LjpegHeader::parse(&jpg, Default::default()).unwrap();
    let mut g = c.benchmark_group("lossless_jpeg");
    g.throughput(Throughput::Elements((w * h) as u64));
    g.bench_function("2mp_14bit_2comp", |b| {
        b.iter(|| {
            let mut s = 0u64;
            ljpeg::decode_rows(&jpg, &hdr, |_, r| {
                s += r[0] as u64;
                Ok(())
            })
            .unwrap();
            black_box(s)
        })
    });
    g.finish();
}

fn develop(c: &mut Criterion) {
    let (w, h) = (4000u32, 3000u32);
    let mut img = RawImage::new_u16(w, h, 1, &Limits::default()).unwrap();
    for (i, v) in img.u16_mut().iter_mut().enumerate() {
        *v = ((i * 7919) % 3900 + 100) as u16;
    }
    let mut info = flashraw::FrameInfo::new(w, h, 12);
    info.cfa = Some(flashraw::CfaPattern::RGGB);
    info.white_level = flashraw::WhiteLevel::uniform(4095);
    let mut g = c.benchmark_group("develop");
    g.throughput(Throughput::Elements((w * h) as u64));
    g.sample_size(10);
    for (name, d) in [("bilinear", flashraw::Demosaic::Bilinear), ("half", flashraw::Demosaic::HalfSize)] {
        let p = flashraw::process::DevelopParams::new(&info, d, flashraw::Rect::full(w, h));
        g.bench_function(format!("rgba8_{name}_12mp"), |b| {
            b.iter(|| black_box(flashraw::process::develop_rgba8(&img, &info, (0, 0), &p, true).unwrap()))
        });
    }
    g.finish();
}

criterion_group!(benches, bit_pumps, unpack, lossless_jpeg, develop);
criterion_main!(benches);
