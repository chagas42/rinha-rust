use serde::Deserialize;

pub const DIM: usize = 14;
pub const DIM_PAD: usize = 16;
pub type Vec14F = [f32; DIM];
pub type Vec14I = [i16; DIM_PAD];

pub const SENTINEL_I16: i16 = -32767;
pub const QUANT_SCALE: f32 = 32767.0;

const MAX_AMOUNT: f32 = 10000.0;
const MAX_INSTALLMENTS: f32 = 12.0;
const AMOUNT_VS_AVG_RATIO: f32 = 10.0;
const MAX_MINUTES: f32 = 1440.0;
const MAX_KM: f32 = 1000.0;
const MAX_TX_COUNT_24H: f32 = 20.0;
const MAX_MERCHANT_AVG_AMOUNT: f32 = 10000.0;

#[derive(Deserialize)]
pub struct FraudRequest<'a> {
    #[serde(borrow)]
    #[allow(dead_code)]
    pub id: &'a str,
    pub transaction: Transaction<'a>,
    pub customer: Customer<'a>,
    pub merchant: Merchant<'a>,
    pub terminal: Terminal,
    pub last_transaction: Option<LastTransaction<'a>>,
}

#[derive(Deserialize)]
pub struct Transaction<'a> {
    pub amount: f32,
    pub installments: u32,
    #[serde(borrow)]
    pub requested_at: &'a str,
}

#[derive(Deserialize)]
pub struct Customer<'a> {
    pub avg_amount: f32,
    pub tx_count_24h: u32,
    #[serde(borrow)]
    pub known_merchants: Vec<&'a str>,
}

#[derive(Deserialize)]
pub struct Merchant<'a> {
    #[serde(borrow)]
    pub id: &'a str,
    #[serde(borrow)]
    pub mcc: &'a str,
    pub avg_amount: f32,
}

#[derive(Deserialize)]
pub struct Terminal {
    pub is_online: bool,
    pub card_present: bool,
    pub km_from_home: f32,
}

#[derive(Deserialize)]
pub struct LastTransaction<'a> {
    #[serde(borrow)]
    #[allow(dead_code)]
    pub timestamp: &'a str,
    pub km_from_current: f32,
}

#[inline(always)]
fn clamp01(x: f32) -> f32 {
    x.max(0.0).min(1.0)
}

pub fn mcc_risk(mcc: &str) -> f32 {
    match mcc {
        "5411" => 0.15,
        "5812" => 0.30,
        "5912" => 0.20,
        "5944" => 0.45,
        "7801" => 0.80,
        "7802" => 0.75,
        "7995" => 0.85,
        "4511" => 0.35,
        "5311" => 0.25,
        "5999" => 0.50,
        _ => 0.50,
    }
}

fn parse_rfc3339_ymdh(s: &str) -> Option<(u32, u32, u32, u32)> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' {
        return None;
    }
    let d = |i: usize| -> Option<u32> {
        let c = b[i];
        if c.is_ascii_digit() { Some((c - b'0') as u32) } else { None }
    };
    let y = d(0)? * 1000 + d(1)? * 100 + d(2)? * 10 + d(3)?;
    let mo = d(5)? * 10 + d(6)?;
    let da = d(8)? * 10 + d(9)?;
    let ho = d(11)? * 10 + d(12)?;
    Some((y, mo, da, ho))
}

fn day_of_week_spec(y: u32, m: u32, d: u32) -> u32 {
    const T: [u32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y_adj = if m < 3 { y - 1 } else { y };
    let sak = (y_adj + y_adj / 4 - y_adj / 100 + y_adj / 400 + T[(m - 1) as usize] + d) % 7;
    (sak + 6) % 7
}

pub fn vectorize(req: &FraudRequest) -> Vec14F {
    let mut v: Vec14F = [0.0; DIM];

    v[0] = clamp01(req.transaction.amount / MAX_AMOUNT);
    v[1] = clamp01(req.transaction.installments as f32 / MAX_INSTALLMENTS);

    v[2] = if req.customer.avg_amount > 0.0 {
        clamp01((req.transaction.amount / req.customer.avg_amount) / AMOUNT_VS_AVG_RATIO)
    } else {
        1.0
    };

    let (y, mo, da, ho) = parse_rfc3339_ymdh(req.transaction.requested_at).unwrap_or((1970, 1, 1, 0));
    v[3] = ho as f32 / 23.0;
    v[4] = day_of_week_spec(y, mo, da) as f32 / 6.0;

    match &req.last_transaction {
        Some(lt) => {
            let minutes = minutes_between(req.transaction.requested_at, lt.timestamp).unwrap_or(0.0);
            v[5] = clamp01(minutes / MAX_MINUTES);
            v[6] = clamp01(lt.km_from_current / MAX_KM);
        }
        None => {
            v[5] = -1.0;
            v[6] = -1.0;
        }
    }

    v[7] = clamp01(req.terminal.km_from_home / MAX_KM);
    v[8] = clamp01(req.customer.tx_count_24h as f32 / MAX_TX_COUNT_24H);
    v[9] = if req.terminal.is_online { 1.0 } else { 0.0 };
    v[10] = if req.terminal.card_present { 1.0 } else { 0.0 };

    let known = req
        .customer
        .known_merchants
        .iter()
        .any(|m| *m == req.merchant.id);
    v[11] = if known { 0.0 } else { 1.0 };

    v[12] = mcc_risk(req.merchant.mcc);
    v[13] = clamp01(req.merchant.avg_amount / MAX_MERCHANT_AVG_AMOUNT);

    v
}

fn minutes_between(now: &str, prev: &str) -> Option<f32> {
    let a = parse_full(now)?;
    let b = parse_full(prev)?;
    let diff = a.checked_sub(b)?;
    Some(diff as f32 / 60.0)
}

fn parse_full(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let d = |i: usize| -> Option<u64> {
        let c = b[i];
        if c.is_ascii_digit() { Some((c - b'0') as u64) } else { None }
    };
    let y = d(0)? * 1000 + d(1)? * 100 + d(2)? * 10 + d(3)?;
    let mo = d(5)? * 10 + d(6)?;
    let da = d(8)? * 10 + d(9)?;
    let h = d(11)? * 10 + d(12)?;
    let mi = d(14)? * 10 + d(15)?;
    let se = d(17)? * 10 + d(18)?;

    let y = y as i64 - (mo <= 2) as i64;
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u64;
    let doy = (153 * (mo + (if mo > 2 { -3i64 as u64 } else { 9 })) + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era as i64 * 146097 + doe as i64 - 719468;
    let secs = days as u64 * 86400 + h * 3600 + mi * 60 + se;
    Some(secs)
}

#[inline(always)]
pub fn quantize(v: &Vec14F) -> Vec14I {
    let mut q: Vec14I = [0; DIM_PAD];
    for i in 0..DIM {
        q[i] = (v[i] * QUANT_SCALE).round() as i16;
    }
    q
}

#[inline(always)]
pub fn l2sq_i16(a: &Vec14I, b: &Vec14I) -> u64 {
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    unsafe {
        return l2sq_i16_avx2(a, b);
    }
    #[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
    {
        l2sq_i16_scalar(a, b)
    }
}

#[inline(always)]
pub fn l2sq_i16_scalar(a: &Vec14I, b: &Vec14I) -> u64 {
    let mut sum: u64 = 0;
    for i in 0..DIM_PAD {
        let d = (a[i] as i32 - b[i] as i32) as i64;
        sum += (d * d) as u64;
    }
    sum
}

#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
#[target_feature(enable = "avx2")]
pub unsafe fn l2sq_i16_avx2(a: &Vec14I, b: &Vec14I) -> u64 {
    use std::arch::x86_64::*;

    let va = _mm256_loadu_si256(a.as_ptr() as *const __m256i);
    let vb = _mm256_loadu_si256(b.as_ptr() as *const __m256i);

    let va_lo = _mm256_cvtepi16_epi32(_mm256_castsi256_si128(va));
    let va_hi = _mm256_cvtepi16_epi32(_mm256_extracti128_si256(va, 1));
    let vb_lo = _mm256_cvtepi16_epi32(_mm256_castsi256_si128(vb));
    let vb_hi = _mm256_cvtepi16_epi32(_mm256_extracti128_si256(vb, 1));

    let d_lo = _mm256_sub_epi32(va_lo, vb_lo);
    let d_hi = _mm256_sub_epi32(va_hi, vb_hi);

    let lo_even = _mm256_mul_epi32(d_lo, d_lo);
    let lo_odd_src = _mm256_srli_epi64(d_lo, 32);
    let lo_odd = _mm256_mul_epi32(lo_odd_src, lo_odd_src);
    let lo_sum = _mm256_add_epi64(lo_even, lo_odd);

    let hi_even = _mm256_mul_epi32(d_hi, d_hi);
    let hi_odd_src = _mm256_srli_epi64(d_hi, 32);
    let hi_odd = _mm256_mul_epi32(hi_odd_src, hi_odd_src);
    let hi_sum = _mm256_add_epi64(hi_even, hi_odd);

    let total256 = _mm256_add_epi64(lo_sum, hi_sum);
    let total_lo = _mm256_castsi256_si128(total256);
    let total_hi = _mm256_extracti128_si256(total256, 1);
    let t = _mm_add_epi64(total_lo, total_hi);
    let high = _mm_unpackhi_epi64(t, t);
    let final_v = _mm_add_epi64(t, high);

    _mm_cvtsi128_si64(final_v) as u64
}

#[inline(always)]
pub fn l2sq_f32(a: &Vec14F, b: &Vec14F) -> f32 {
    let mut sum: f32 = 0.0;
    for i in 0..DIM {
        let d = a[i] - b[i];
        sum += d * d;
    }
    sum
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable, Debug)]
pub struct IndexHeader {
    pub magic: u32,
    pub version: u32,
    pub n_vectors: u32,
    pub n_centroids: u32,
    pub dim: u32,
    pub _pad: [u32; 3],
}

pub const INDEX_MAGIC: u32 = 0x52_49_4E_48;
pub const INDEX_VERSION: u32 = 2;

pub struct IndexView<'a> {
    pub centroids: &'a [Vec14F],
    pub offsets: &'a [u32],
    pub vectors: &'a [Vec14I],
    pub labels: &'a [u8],
}

impl<'a> IndexView<'a> {
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, &'static str> {
        let header_size = std::mem::size_of::<IndexHeader>();
        if bytes.len() < header_size {
            return Err("index file too small for header");
        }
        let header: &IndexHeader = bytemuck::from_bytes(&bytes[..header_size]);
        if header.magic != INDEX_MAGIC {
            return Err("invalid index magic");
        }
        if header.version != INDEX_VERSION {
            return Err("unsupported index version");
        }
        if header.dim as usize != DIM {
            return Err("index dimension mismatch");
        }
        let n = header.n_vectors as usize;
        let k = header.n_centroids as usize;

        let mut off = header_size;
        let centroids_bytes = k * DIM * std::mem::size_of::<f32>();
        let centroids: &[Vec14F] =
            bytemuck::cast_slice(&bytes[off..off + centroids_bytes]);
        off += centroids_bytes;

        let offsets_bytes = (k + 1) * std::mem::size_of::<u32>();
        let offsets: &[u32] = bytemuck::cast_slice(&bytes[off..off + offsets_bytes]);
        off += offsets_bytes;

        let vectors_bytes = n * DIM_PAD * std::mem::size_of::<i16>();
        let vectors: &[Vec14I] =
            bytemuck::cast_slice(&bytes[off..off + vectors_bytes]);
        off += vectors_bytes;

        let labels_bytes = (n + 7) / 8;
        let labels: &[u8] = &bytes[off..off + labels_bytes];

        Ok(IndexView {
            centroids,
            offsets,
            vectors,
            labels,
        })
    }
}

#[inline(always)]
fn label_is_fraud(labels: &[u8], idx: usize) -> bool {
    labels[idx / 8] & (1 << (idx % 8)) != 0
}

#[inline(always)]
fn insert_top5(top: &mut [(u64, u32); 5], d: u64, idx: u32) {
    if d >= top[4].0 {
        return;
    }
    let mut j = 4usize;
    while j > 0 && top[j - 1].0 > d {
        top[j] = top[j - 1];
        j -= 1;
    }
    top[j] = (d, idx);
}

pub fn ivf_search_2stage(
    idx: &IndexView,
    query: &Vec14F,
    nprobe_primary: usize,
    nprobe_refine: usize,
) -> u8 {
    let frauds = ivf_search(idx, query, nprobe_primary);
    if frauds == 2 || frauds == 3 {
        ivf_search(idx, query, nprobe_refine)
    } else {
        frauds
    }
}

pub fn ivf_search(idx: &IndexView, query: &Vec14F, nprobe: usize) -> u8 {
    let qi = quantize(query);

    let k = idx.centroids.len();
    debug_assert!(nprobe > 0 && nprobe <= 64);
    let mut probes: [(f32, u32); 64] = [(f32::INFINITY, u32::MAX); 64];
    let np = nprobe.min(64);
    for c in 0..k {
        let d = l2sq_f32(query, &idx.centroids[c]);
        if d < probes[np - 1].0 {
            let mut j = np - 1;
            while j > 0 && probes[j - 1].0 > d {
                probes[j] = probes[j - 1];
                j -= 1;
            }
            probes[j] = (d, c as u32);
        }
    }

    let mut top: [(u64, u32); 5] = [(u64::MAX, u32::MAX); 5];
    for p in 0..np {
        let c = probes[p].1 as usize;
        let start = idx.offsets[c] as usize;
        let end = idx.offsets[c + 1] as usize;
        for i in start..end {
            let d = l2sq_i16(&qi, &idx.vectors[i]);
            insert_top5(&mut top, d, i as u32);
        }
    }

    let mut frauds: u8 = 0;
    for &(_, vidx) in &top {
        if vidx != u32::MAX && label_is_fraud(idx.labels, vidx as usize) {
            frauds += 1;
        }
    }
    frauds
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round4(v: Vec14F) -> Vec14F {
        let mut out = v;
        for x in out.iter_mut() {
            *x = (*x * 10000.0).round() / 10000.0;
        }
        out
    }

    #[test]
    fn legit_example_from_spec() {
        let json = br#"{
            "id": "tx-1329056812",
            "transaction": { "amount": 41.12, "installments": 2, "requested_at": "2026-03-11T18:45:53Z" },
            "customer": { "avg_amount": 82.24, "tx_count_24h": 3, "known_merchants": ["MERC-003", "MERC-016"] },
            "merchant": { "id": "MERC-016", "mcc": "5411", "avg_amount": 60.25 },
            "terminal": { "is_online": false, "card_present": true, "km_from_home": 29.23 },
            "last_transaction": null
        }"#;
        let req: FraudRequest = serde_json::from_slice(json).unwrap();
        let got = round4(vectorize(&req));
        let expect = [0.0041, 0.1667, 0.05, 0.7826, 0.3333, -1.0, -1.0, 0.0292, 0.15, 0.0, 1.0, 0.0, 0.15, 0.006];
        for i in 0..DIM {
            let diff = (got[i] - expect[i]).abs();
            assert!(diff < 0.0002, "dim {} got {} expected {} (diff {})", i, got[i], expect[i], diff);
        }
    }

    #[test]
    fn fraud_example_from_spec() {
        let json = br#"{
            "id": "tx-3330991687",
            "transaction": { "amount": 9505.97, "installments": 10, "requested_at": "2026-03-14T05:15:12Z" },
            "customer": { "avg_amount": 81.28, "tx_count_24h": 20, "known_merchants": ["MERC-008", "MERC-007", "MERC-005"] },
            "merchant": { "id": "MERC-068", "mcc": "7802", "avg_amount": 54.86 },
            "terminal": { "is_online": false, "card_present": true, "km_from_home": 952.27 },
            "last_transaction": null
        }"#;
        let req: FraudRequest = serde_json::from_slice(json).unwrap();
        let got = round4(vectorize(&req));
        let expect = [0.9506, 0.8333, 1.0, 0.2174, 0.8333, -1.0, -1.0, 0.9523, 1.0, 0.0, 1.0, 1.0, 0.75, 0.0055];
        for i in 0..DIM {
            let diff = (got[i] - expect[i]).abs();
            assert!(diff < 0.0002, "dim {} got {} expected {} (diff {})", i, got[i], expect[i], diff);
        }
    }

    #[test]
    fn quantize_roundtrip_and_sentinel() {
        let v: Vec14F = [0.0, 1.0, 0.5, -1.0, 0.0041, 0.1667, 0.05, 0.7826, 0.3333, -1.0, 0.0, 0.5, 0.99999, 0.001];
        let q = quantize(&v);
        assert_eq!(q[0], 0);
        assert_eq!(q[1], QUANT_SCALE as i16);
        assert_eq!(q[3], SENTINEL_I16);
        assert_eq!(q[9], SENTINEL_I16);
        for i in DIM..DIM_PAD {
            assert_eq!(q[i], 0, "padding lane {} not zero", i);
        }
    }

    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    #[test]
    fn simd_matches_scalar_random() {
        let mut state: u64 = 0xDEADBEEF;
        let mut next = || -> i16 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 33) as i32 - i16::MAX as i32 / 2) as i16
        };
        for _ in 0..200 {
            let mut a: Vec14I = [0; DIM_PAD];
            let mut b: Vec14I = [0; DIM_PAD];
            for i in 0..DIM { a[i] = next(); b[i] = next(); }
            let scalar = l2sq_i16_scalar(&a, &b);
            let simd = unsafe { l2sq_i16_avx2(&a, &b) };
            assert_eq!(scalar, simd, "mismatch a={:?} b={:?}", a, b);
        }
    }

    #[test]
    fn day_of_week_known_dates() {
        assert_eq!(day_of_week_spec(2026, 3, 11), 2);
        assert_eq!(day_of_week_spec(2026, 3, 14), 5);
        assert_eq!(day_of_week_spec(2026, 3, 9), 0);
        assert_eq!(day_of_week_spec(2026, 3, 15), 6);
    }

    #[test]
    fn index_view_roundtrip() {
        let n = 4usize;
        let k = 2usize;
        let header = IndexHeader {
            magic: INDEX_MAGIC,
            version: INDEX_VERSION,
            n_vectors: n as u32,
            n_centroids: k as u32,
            dim: DIM as u32,
            _pad: [0; 3],
        };
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(bytemuck::bytes_of(&header));
        let centroids: Vec<Vec14F> = vec![[0.1; DIM], [0.9; DIM]];
        for c in &centroids {
            buf.extend_from_slice(bytemuck::cast_slice::<f32, u8>(c));
        }
        let offsets: Vec<u32> = vec![0, 2, 4];
        buf.extend_from_slice(bytemuck::cast_slice::<u32, u8>(&offsets));
        let vectors: Vec<Vec14I> = vec![[1; DIM_PAD], [2; DIM_PAD], [3; DIM_PAD], [4; DIM_PAD]];
        for v in &vectors {
            buf.extend_from_slice(bytemuck::cast_slice::<i16, u8>(v));
        }
        let bitmap: Vec<u8> = vec![0b0000_1010];
        buf.extend_from_slice(&bitmap);

        let view = IndexView::from_bytes(&buf).expect("parse roundtrip");
        assert_eq!(view.centroids.len(), k);
        assert_eq!(view.offsets, &[0u32, 2, 4]);
        assert_eq!(view.vectors.len(), n);
        assert_eq!(view.vectors[2], [3i16; DIM_PAD]);
        assert!(!label_is_fraud(view.labels, 0));
        assert!(label_is_fraud(view.labels, 1));
        assert!(!label_is_fraud(view.labels, 2));
        assert!(label_is_fraud(view.labels, 3));
    }

    #[test]
    fn index_view_rejects_bad_magic() {
        let bad = vec![0u8; std::mem::size_of::<IndexHeader>()];
        assert!(IndexView::from_bytes(&bad).is_err());
    }
}
