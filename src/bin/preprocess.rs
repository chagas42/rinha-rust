use std::env;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::time::Instant;

use flate2::read::GzDecoder;
use rand::seq::SliceRandom;
use rand::{rngs::StdRng, SeedableRng};
use rayon::prelude::*;

use rinha_2026::{
    l2sq_f32, quantize, IndexHeader, Vec14F, Vec14I, DIM, INDEX_MAGIC, INDEX_VERSION,
};

#[derive(serde::Deserialize)]
struct RefRecord {
    vector: [f32; DIM],
    label: String,
}

fn main() {
    let mut args = env::args().skip(1);
    let in_path = args.next().expect("usage: preprocess <refs.json.gz> <out.bin> [n_centroids] [iters]");
    let out_path = args.next().expect("usage: preprocess <refs.json.gz> <out.bin> [n_centroids] [iters]");
    let n_centroids: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(1024);
    let kmeans_iters: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(10);

    let t0 = Instant::now();
    eprintln!("[preprocess] reading {}", in_path);
    let file = File::open(&in_path).expect("open input");
    let gz = GzDecoder::new(file);
    let reader = BufReader::with_capacity(8 * 1024 * 1024, gz);

    let records: Vec<RefRecord> = serde_json::from_reader(reader).expect("parse json");
    let n = records.len();
    eprintln!("[preprocess] read {} records ({:.2}s)", n, t0.elapsed().as_secs_f32());

    let mut vectors: Vec<Vec14F> = Vec::with_capacity(n);
    let mut labels: Vec<bool> = Vec::with_capacity(n);
    for r in records {
        vectors.push(r.vector);
        labels.push(r.label == "fraud");
    }
    let frauds = labels.iter().filter(|&&l| l).count();
    eprintln!(
        "[preprocess] {} fraud / {} legit ({:.2}% fraud)",
        frauds,
        n - frauds,
        100.0 * frauds as f32 / n as f32
    );

    eprintln!(
        "[preprocess] k-means K={} iters={}",
        n_centroids, kmeans_iters
    );
    let mut rng = StdRng::seed_from_u64(42);
    let mut centroids: Vec<Vec14F> = vectors
        .choose_multiple(&mut rng, n_centroids)
        .copied()
        .collect();

    let mut assignments: Vec<u32> = vec![0; n];

    for it in 0..kmeans_iters {
        let t = Instant::now();

        assignments
            .par_iter_mut()
            .enumerate()
            .for_each(|(i, a)| {
                let v = &vectors[i];
                let mut best_c: u32 = 0;
                let mut best_d = f32::INFINITY;
                for (c, ck) in centroids.iter().enumerate() {
                    let d = l2sq_f32(v, ck);
                    if d < best_d {
                        best_d = d;
                        best_c = c as u32;
                    }
                }
                *a = best_c;
            });

        let mut sums: Vec<[f64; DIM]> = vec![[0.0; DIM]; n_centroids];
        let mut counts: Vec<u64> = vec![0; n_centroids];
        for (v, &a) in vectors.iter().zip(assignments.iter()) {
            let s = &mut sums[a as usize];
            for d in 0..DIM {
                s[d] += v[d] as f64;
            }
            counts[a as usize] += 1;
        }
        for c in 0..n_centroids {
            if counts[c] > 0 {
                let inv = 1.0 / counts[c] as f64;
                for d in 0..DIM {
                    centroids[c][d] = (sums[c][d] * inv) as f32;
                }
            }
        }

        eprintln!(
            "[preprocess] kmeans iter {}/{} in {:.2}s",
            it + 1,
            kmeans_iters,
            t.elapsed().as_secs_f32()
        );
    }

    eprintln!("[preprocess] final assignment");
    assignments
        .par_iter_mut()
        .enumerate()
        .for_each(|(i, a)| {
            let v = &vectors[i];
            let mut best_c: u32 = 0;
            let mut best_d = f32::INFINITY;
            for (c, ck) in centroids.iter().enumerate() {
                let d = l2sq_f32(v, ck);
                if d < best_d {
                    best_d = d;
                    best_c = c as u32;
                }
            }
            *a = best_c;
        });

    eprintln!("[preprocess] sort + offsets");
    let mut order: Vec<u32> = (0..n as u32).collect();
    order.par_sort_unstable_by_key(|&i| assignments[i as usize]);

    let mut offsets: Vec<u32> = vec![0; n_centroids + 1];
    for &a in assignments.iter() {
        offsets[a as usize + 1] += 1;
    }
    for i in 1..=n_centroids {
        offsets[i] += offsets[i - 1];
    }

    eprintln!("[preprocess] quantize + reorder");
    let mut q_vectors: Vec<Vec14I> = Vec::with_capacity(n);
    let mut sorted_labels: Vec<bool> = Vec::with_capacity(n);
    for &idx in &order {
        q_vectors.push(quantize(&vectors[idx as usize]));
        sorted_labels.push(labels[idx as usize]);
    }

    let mut bitmap: Vec<u8> = vec![0; (n + 7) / 8];
    for (i, &l) in sorted_labels.iter().enumerate() {
        if l {
            bitmap[i / 8] |= 1 << (i % 8);
        }
    }

    eprintln!("[preprocess] writing {}", out_path);
    let out = File::create(&out_path).expect("create output");
    let mut w = BufWriter::with_capacity(8 * 1024 * 1024, out);

    let header = IndexHeader {
        magic: INDEX_MAGIC,
        version: INDEX_VERSION,
        n_vectors: n as u32,
        n_centroids: n_centroids as u32,
        dim: DIM as u32,
        _pad: [0; 3],
    };
    w.write_all(bytemuck::bytes_of(&header)).unwrap();

    for c in &centroids {
        w.write_all(bytemuck::cast_slice::<f32, u8>(c)).unwrap();
    }

    w.write_all(bytemuck::cast_slice::<u32, u8>(&offsets))
        .unwrap();

    for v in &q_vectors {
        w.write_all(bytemuck::cast_slice::<i16, u8>(v)).unwrap();
    }

    w.write_all(&bitmap).unwrap();

    w.flush().unwrap();
    drop(w);
    eprintln!(
        "[preprocess] done in {:.2}s",
        t0.elapsed().as_secs_f32()
    );
    std::process::exit(0);
}
