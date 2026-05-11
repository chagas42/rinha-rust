use std::env;
use std::fs::File;
use std::io::Read;
use std::time::Instant;

use memmap2::Mmap;

use rinha_2026::{ivf_search, ivf_search_2stage, vectorize, FraudRequest, IndexView};

#[derive(serde::Deserialize)]
struct TestData<'a> {
    #[serde(borrow)]
    entries: Vec<Entry<'a>>,
    stats: Stats,
}

#[derive(serde::Deserialize)]
struct Entry<'a> {
    #[serde(borrow)]
    request: FraudRequest<'a>,
    expected_approved: bool,
    #[allow(dead_code)]
    expected_fraud_score: f64,
}

#[derive(serde::Deserialize, Debug)]
struct Stats {
    #[allow(dead_code)]
    total: u32,
    fraud_count: u32,
    legit_count: u32,
}

fn main() {
    let mut args = env::args().skip(1);
    let index_path = args.next().expect("usage: quality_check <index.bin> <test-data.json> [nprobe]");
    let test_path = args.next().expect("usage: quality_check <index.bin> <test-data.json> [nprobe]");
    let nprobe: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(16);
    let nprobe_refine: Option<usize> = args.next().and_then(|s| s.parse().ok());

    let mode = match nprobe_refine {
        Some(r) => format!("2-stage primary={} refine={}", nprobe, r),
        None => format!("single nprobe={}", nprobe),
    };
    eprintln!("[quality] index={} test={} mode={}", index_path, test_path, mode);

    let file = File::open(&index_path).expect("open index.bin");
    let mmap = unsafe { Mmap::map(&file).expect("mmap index.bin") };
    let idx = IndexView::from_bytes(mmap.as_ref()).expect("parse index.bin");
    eprintln!("[quality] index loaded: {} vectors, {} centroids", idx.vectors.len(), idx.centroids.len());

    let mut buf = Vec::new();
    File::open(&test_path).expect("open test-data").read_to_end(&mut buf).expect("read");
    eprintln!("[quality] test-data: {} MB", buf.len() / (1024 * 1024));

    let parse_t = Instant::now();
    let data: TestData = serde_json::from_slice(&buf).expect("parse test-data");
    eprintln!(
        "[quality] parsed {} entries in {:.2}s (ground truth: {} fraud, {} legit)",
        data.entries.len(), parse_t.elapsed().as_secs_f32(),
        data.stats.fraud_count, data.stats.legit_count
    );

    let run_t = Instant::now();
    let mut tp: u64 = 0;
    let mut tn: u64 = 0;
    let mut fp: u64 = 0;
    let mut fn_: u64 = 0;

    for e in &data.entries {
        let v = vectorize(&e.request);
        let frauds = match nprobe_refine {
            Some(r) => ivf_search_2stage(&idx, &v, nprobe, r),
            None => ivf_search(&idx, &v, nprobe),
        };
        let approved = frauds < 3;

        match (e.expected_approved, approved) {
            (true, true)   => tn += 1,
            (false, false) => tp += 1,
            (true, false)  => fp += 1,
            (false, true)  => fn_ += 1,
        }
    }

    let elapsed = run_t.elapsed();
    let n = data.entries.len() as u64;
    let mean_us = elapsed.as_secs_f64() * 1_000_000.0 / n as f64;

    let errs: u64 = 0;
    let weighted_e = fp * 1 + fn_ * 3 + errs * 5;
    let failures = fp + fn_ + errs;
    let failure_rate = failures as f64 / n as f64;
    let epsilon = weighted_e as f64 / n as f64;

    let k = 1000.0f64;
    let epsilon_min = 0.001f64;
    let beta = 300.0f64;
    let rate_component = k * (1.0 / epsilon.max(epsilon_min)).log10();
    let absolute_penalty = -beta * (1.0 + weighted_e as f64).log10();
    let cut_triggered = failure_rate > 0.15;
    let score_det = if cut_triggered { -3000.0 } else { rate_component + absolute_penalty };

    println!();
    println!("=== Quality report ({}) ===", mode);
    println!("entries:        {}", n);
    println!("mean lookup:    {:.2} µs  (total {:.2}s)", mean_us, elapsed.as_secs_f64());
    println!();
    println!("confusion matrix (per Rinha terminology):");
    println!("  TP (fraud correctly denied):  {}", tp);
    println!("  TN (legit correctly approved): {}", tn);
    println!("  FP (legit denied):             {}", fp);
    println!("  FN (fraud approved):           {}", fn_);
    println!();
    println!("failures:       {} ({:.3}%)", failures, failure_rate * 100.0);
    println!("weighted E:     {}", weighted_e);
    println!("epsilon:        {:.6}", epsilon);
    println!();
    println!("estimated score_det:    {:.2} pts  (rate {:.2}, penalty {:.2})",
        score_det, rate_component, absolute_penalty);
    if cut_triggered {
        println!("  cut triggered (failure rate > 15%)");
    }
}
