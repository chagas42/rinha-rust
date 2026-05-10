---
name: review-rinha
description: Code review especializado pra esse projeto Rust de fraud detection (Rinha de Backend 2026). Cobre 12 categorias (hot path zero-alloc, unsafe blocks, SIMD AVX2, mmap+Box::leak, Tokio current_thread, build profile, Docker multi-stage, IVF tuning, quality vs ground truth, profiling, cache patterns, spec compliance). Compara escolhas locais contra os top performers Rust observados no ranking público da Rinha 2026. Use quando o usuário pedir review, análise, ou auditoria do código, ou ao analisar um commit/PR antes de submeter.
allowed-tools: Read, Grep, Glob, Bash(git *), Bash(cargo *), Bash(docker compose *), Bash(./target/release/quality_check *), WebFetch
---

# Review do projeto Rinha 2026 (Rust + IVF + SIMD)

Você está revisando um backend Rust performance-critical pra o desafio Rinha de Backend 2026. **Alvo: top 3 absoluto, p99 < 1ms no Mac Mini Late 2014 (Haswell i7-4578U, AVX2 nativo)**.

## Contexto do projeto (essencial)

- **Stack confirmado**: Rust 2021 + hyper 1.x + tokio (current_thread) + simd-json + memmap2 + HAProxy 2.9 via Unix Domain Sockets
- **Algoritmo**: IVF com K=1024 centroides, nprobe=32, k-NN com k=5, distância Euclidiana
- **Quantização**: int16 com escala `[-1, 1] → [-32767, 32767]`, padding de 14→16 dims pra SIMD AVX2
- **SIMD**: `_mm256_sub_epi16` + `_mm256_madd_epi16` + reduce horizontal; fallback escalar em ARM
- **Memória**: `index.bin` (96 MB) gerado em build time, carregado via `mmap` + `Box::leak` no startup
- **Orçamento**: 1 CPU agregado (0.40 + 0.40 + 0.20) + 350 MB agregado (150 + 150 + 50)
- **Score atual estimado**: ~+2700 latência + ~+2857 detecção (medido offline em 54.100 transações com nprobe=32)

Arquivos principais a auditar:
- `src/lib.rs` — vetorização (14 dims), quantização, IVF search, IndexView/IndexHeader, 4 unit tests
- `src/main.rs` — hyper server, mmap, pre-render das 6 respostas possíveis, dispatch TCP/UDS
- `src/bin/preprocess.rs` — k-means + serialização do índice (v2 formato padded)
- `src/bin/quality_check.rs` — bench offline contra `test-data.json` (calcula TP/TN/FP/FN + score_det)
- `Cargo.toml` — deps + 3 bins
- `Dockerfile` — multi-stage com preprocess no build, `target-cpu=haswell`
- `docker-compose.yml` — 2 APIs + HAProxy, limits exatos
- `haproxy.cfg` — round-robin via UDS

## Procedimento recomendado

1. **Snapshot inicial**: rode `git log --oneline -10` e `git status` pra contextualizar o estado.
2. **Build limpo**: `cargo check` + `cargo test --lib` precisam passar verdes. Se algum teste falhar, **bloqueia o review**.
3. **Walkthrough estrutural**: leia os arquivos na ordem acima. Compare cada bloco com a categoria de checklist correspondente abaixo.
4. **Mudanças desde o último review**: `git diff <ref>..HEAD -- src/` (ou diff do PR se aplicável). Foque nesse diff.
5. **Quality check empírico**: rode `./target/release/quality_check /tmp/index.bin /Users/chagas42/@studies/rinha-de-backend-2026/test/test-data.json 32` pra confirmar que mudanças não destruíram detecção.
6. **Smoke local**: `docker compose up -d && curl -s http://localhost:9999/ready && docker compose down` — stack precisa subir sem OOM.
7. **Reporte estruturado**: bullets agrupados por categoria, cada item com (a) o que está, (b) por que importa, (c) sugestão acionável.

## 12 categorias de auditoria

Pra cada uma, há **o que olhar**, **por que importa**, **sinais de alerta**, e **comparação com top performers Rust da Rinha 2026**.

### 1. Hot path zero-alloc

**O que olhar**: `handle()` em `src/main.rs` — toda alocação por request mata p99. Procure `String::new`, `vec![...]`, `to_string()`, `to_owned()`, `format!`, `clone()`, `collect::<Vec<_>>` no caminho da request.

**Por que**: cada alocação custa centenas de ns + lock no allocator global. Em 900 RPS sustentado, allocs por request inflam p99 em ms.

**Sinais de alerta**:
- `let mut buf = Vec::with_capacity(N)` chamado por request (deveria ser reaproveitado via `thread_local!` ou `clear()`)
- `format!()` ou `to_string()` em response building
- `HashMap` em vez de array fixo quando os índices são pequenos e conhecidos

**Padrão dos top**: davidalecrim1 mantém scratch arrays fixos; MXLange usa `&'static` buffer; ninguém aloca por request.

**Estado atual**: pre-render das 6 respostas resolve serialize alloc. simd-json aloca `Vec<u8>` de ~500 bytes por request (body buffer com padding) — aceitável mas marginal. **Verificar se simd-json não está alocando algo escondido com payload grande.**

### 2. `unsafe` blocks

**O que olhar**: cada `unsafe` deve ter (a) comentário `// SAFETY: ...` explicando invariantes, (b) ser contido (não exposto na API pública sem motivo), (c) idealmente passar em `cargo miri test` (não temos miri configurado — sugerir).

**Por que**: UB silencioso em SIMD/mmap é a fonte mais comum de bugs em produção Rust. Miri detecta dinamicamente.

**Sinais de alerta**:
- `unsafe fn` público que aceita slice sem validar tamanho
- `from_raw_parts(ptr, n)` com cast de tipo (se tipos têm tamanho diferente, é UB)
- `Mmap::map` sem garantia que o arquivo não é modificado em paralelo

**Estado atual**: temos 3 `unsafe`: `Mmap::map` (lifetime do mmap, conhecido), `l2sq_i16_avx2` (target_feature, justificado), `Box::leak(Box::new(mmap))` (vazamento intencional). Todos comentados — bom. **Sugerir adicionar `cargo miri test` no CI.**

### 3. SIMD AVX2

**O que olhar**: `lib.rs::l2sq_i16_avx2` — atributo `#[target_feature(enable = "avx2")]` obrigatório, alinhamento de slice (i16 = 2 bytes, AVX2 prefere 32-byte alignment mas `_mm256_loadu_si256` aceita unaligned).

**Por que**: sem `target_feature`, compilador não emite vetorização. Sem alinhamento, perde-se ~10% de throughput em alguns CPUs.

**Sinais de alerta**:
- Uso de `_mm256_*` em função normal sem `#[target_feature]`
- Uso de `_mm256_load_si256` (aligned) sem garantir alinhamento — usar `loadu` se incerto
- Acumular `i16 * i16 → i16` (overflow). Sempre `i32`.

**Padrão dos top**: MXLange tem o kernel mais limpo (`_mm256_sub_epi16` + `_mm256_madd_epi16` + tree-reduce) — idêntico ao nosso. davidalecrim1 também usa AVX2 manual.

**Estado atual**: `l2sq_i16_avx2` é canônico, com reduce horizontal correto. **Verificar se há teste `simd_matches_scalar_random` rodando em x86_64+avx2** (atualmente cfg-gated; só roda em build amd64 com target-feature, não em dev ARM).

### 4. mmap + `Box::leak`

**O que olhar**: `main.rs::load_index_static` — `Box::leak` torna o mmap `'static`, evitando `Arc<Mmap>`. mmap conta no cgroup como page cache!

**Por que**: page cache do mmap entra no `memory.usage_in_bytes` do cgroup. Com 150 MB de limit e 96 MB de índice, qualquer outra alocação grande dispara OOM.

**Sinais de alerta**:
- Warmup ativo (tocar todas as páginas no startup) — **causou OOM no nosso caso, foi revertido**
- `mlockall` sem `RLIMIT_MEMLOCK` aumentado — pode falhar silenciosamente
- Falta de `madvise(MADV_RANDOM)` num cenário de random access — read-ahead desperdiça page cache

**Padrão dos top**: daniloitagyba usa mmap (igual a nós). Maioria usa `include_bytes!` (embedded no binário). Trade-off:
- `include_bytes!`: 0 paging, binário gordo (96+ MB), build mais lento.
- `mmap`: lazy paging, binário slim, mais propenso a OOM em pressure.

**Estado atual**: mmap puro sem `madvise` nem warmup. Sob 1 CPU constrained, kernel pode pinar páginas sob load — vai funcionar mas com possíveis page faults iniciais. **Sugestão**: testar `madvise(MADV_RANDOM)` se possível; comparar com `include_bytes!` se o build aguentar.

### 5. Tokio runtime

**O que olhar**: `main.rs` — `#[tokio::main(flavor = "current_thread")]` obrigatório em 1 CPU constrained. Verificar se não há `tokio::spawn` desnecessário em hot path.

**Por que**: multi_thread runtime em 1 vCPU paga work-stealing sem ganho.

**Sinais de alerta**:
- `#[tokio::main]` default (multi_thread)
- `std::sync::Mutex` segurado entre `await`s
- `#[tracing::instrument]` no handler crítico (allocations + atomics escondidos)
- `tokio::spawn` por request quando tudo cabe na mesma task

**Padrão dos top**: 8/8 usam single-thread (`worker_threads=1` ou `current_thread`).

**Estado atual**: `current_thread` configurado. Apenas 1 `tokio::spawn` por conexão TCP (não por request) — necessário pro accept loop. **OK.**

### 6. Build profile (`Cargo.toml`)

**O que olhar**: `[profile.release]` precisa ter `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = true`.

**Por que**: LTO fat + codegen-units=1 permite inlining cross-crate (nosso `l2sq_i16_avx2` em lib pode ser inlinado no main). `panic = abort` sem landing pads em hot path. `strip` reduz binário (relevante pro Docker image size).

**Sinais de alerta**:
- `lto = "thin"` ou ausente
- `codegen-units` default (16)
- `panic = "unwind"` (default)

**Estado atual**: ✅ tudo correto.

### 7. Docker / build

**O que olhar**: `Dockerfile` multi-stage com `--platform=linux/amd64`, `RUSTFLAGS="-C target-cpu=haswell"` (ativa AVX2 + BMI2), runtime stage debian-slim ou distroless.

**Por que**: target-cpu errado = sem AVX2 = SIMD volta a escalar = 3-4× mais lento.

**Sinais de alerta**:
- Sem `--platform=linux/amd64` (em Mac ARM, vira aarch64 silenciosamente)
- `target-cpu` ausente → sai sem AVX2
- Image base `rust:1.x` no runtime (1 GB+)
- `USER rinha` mantido quando precisa criar UDS em volume (causa permission denied)

**Padrão dos top**: 5/8 usam `FROM scratch` musl estático. Image final ~MB.

**Estado atual**: `--platform=linux/amd64` + `target-cpu=haswell` ✅. Runtime base `debian:bookworm-slim` (~80 MB) — mais pesado que ideal mas funciona. **Sugestão pro futuro**: migrar pra `FROM scratch` musl reduz imagem pra ~20 MB. Não urgente.

### 8. HTTP/JSON parsing

**O que olhar**: hyper low-level (não axum), simd-json com buffer mutável + 32 bytes de padding, pre-render de resposta.

**Por que**: axum tem overhead de extractors em sub-ms target. simd-json em hot path requer buffer pre-alocado pra ganhar.

**Sinais de alerta**:
- axum em vez de hyper raw (ou bench mostrando ganho mensurável de axum)
- `serde_json::to_vec` em resposta (deveria ser pre-render dos 6 byte arrays)
- `req.body().to_vec()` extra-alocando

**Padrão dos top**: davidalecrim1, Ismaellima4, toto9in usam axum. daniloitagyba, MXLange usam hyper raw. Nosso projeto: hyper raw (escolha legítima pra hot path).

**Estado atual**: ✅ hyper raw + simd-json + pre-render das 6 respostas.

### 9. Vector search tuning (IVF / nprobe / K)

**O que olhar**: `lib.rs::ivf_search` + `bin/preprocess.rs::main`. Constantes K (centroides), nprobe (probe count), kmeans_iters.

**Por que**: trade-off latência ↔ recall. Sweet spot empírico determina score_det vs score_p99.

**Sinais de alerta**:
- Brute force em N=3M (só passa em 100K-ish)
- K muito pequeno (< 256) ou muito grande (> 4096)
- nprobe = K (= brute force)
- kmeans_iters < 5 (índice ruim)

**Padrão dos top**: davidalecrim1-extreme usa **2-stage IVF**: nprobe=4 primary + nprobe=24 refine só quando voto ambíguo (2/5 ou 3/5 fraudes). **Pode ser ganho significativo pra nós** — investigar em algum momento.

**Estado atual**: K=1024 + nprobe=32 single-stage + 10 iters. Quality_check mostra zero FN e 2 FP em 54k. **Score_det estimado 2857**. Sweet spot razoável; refine ambíguo poderia subir mais.

### 10. Detection quality (vs ground truth)

**O que olhar**: rode `./target/release/quality_check /tmp/index.bin /Users/chagas42/@studies/rinha-de-backend-2026/test/test-data.json 32` periodicamente. Tabela esperada:

```
TP (fraude negada):   ~24056
TN (legit aprovado):  ~30039
FP (legit negado):    < 5
FN (fraude aprovada): < 3
failure rate:         < 0.05%
score_det estimado:   > 2700
```

**Por que**: o eixo de detecção pesa igual ao p99 no score final. Otimizar latência destruindo accuracy é prejuízo líquido.

**Sinais de alerta**:
- FN > 5 (frauds escapando — peso 3× cada)
- failure rate > 1% (longe do cut-off de 15% mas começa a punir o score)
- Mudança no diff que afeta `vectorize()` sem rodar quality_check de novo

**Estado atual**: zero FN, 2 FP, 0.004% — ótimo.

### 11. Reprodutibilidade

**O que olhar**: seed fixa no k-means (`StdRng::seed_from_u64(42)` no preprocess), iteração determinística (sem `HashMap` no build do índice), golden test do índice (hash do `index.bin`).

**Por que**: sem reprodutibilidade, dois `docker compose build` consecutivos geram índices ligeiramente diferentes → recall varia entre deploys → score flutua.

**Sinais de alerta**:
- `rand::thread_rng()` no preprocess
- Iteração de `HashMap` na construção dos clusters
- Sem assertion de checksum do `index.bin` no CI

**Padrão dos top**: davidalecrim1 documenta "stable packed-row order" pra preservar IDs em tie-break.

**Estado atual**: seed fixa ✅, sem HashMap no preprocess ✅. **Sem golden test (hash sha256 do index.bin) — sugestão de adicionar.**

### 12. Spec compliance

**O que olhar**: `vectorize()` em `lib.rs` deve reproduzir EXATAMENTE os exemplos da `docs/en/DETECTION_RULES.md`. Threshold 0.6. Score = count/5. Sentinela -1 nos índices 5 e 6 quando `last_transaction: null`. Resposta `{approved, fraud_score}` em JSON.

**Por que**: qualquer desvio gera FP/FN não pela lógica de ANN mas por bug de implementação.

**Sinais de alerta**:
- Testes unitários `legit_example_from_spec` ou `fraud_example_from_spec` falhando
- Threshold escrito como `0.5` em vez de `0.6`
- Sentinela `0` em vez de `-1` quando null
- `day_of_week` sem rotação (Sakamoto retorna sun=0; spec quer mon=0)

**Estado atual**: 4 testes verdes ✅, smoke legit→0.0 e fraud→0.8/1.0 conforme spec ✅.

## Comandos úteis (use durante o review)

```bash
# Validar build
cargo check
cargo test --lib

# Quality check empírico
./target/release/quality_check /tmp/index.bin /Users/chagas42/@studies/rinha-de-backend-2026/test/test-data.json 32

# Sweep nprobe pra ver o trade-off
for np in 4 8 16 32 64; do
  echo "--- nprobe=$np ---"
  ./target/release/quality_check /tmp/index.bin /Users/chagas42/@studies/rinha-de-backend-2026/test/test-data.json $np 2>&1 | grep -E "(mean lookup|FP|FN|score_det)"
done

# Smoke + bench de latência (compose precisa estar up)
docker compose up -d
until curl -sf http://localhost:9999/ready; do sleep 0.5; done

PAYLOAD='{"id":"tx-bench","transaction":{"amount":384.88,"installments":3,"requested_at":"2026-03-11T20:23:35Z"},"customer":{"avg_amount":769.76,"tx_count_24h":3,"known_merchants":["MERC-009","MERC-001"]},"merchant":{"id":"MERC-001","mcc":"5912","avg_amount":298.95},"terminal":{"is_online":false,"card_present":true,"km_from_home":13.71},"last_transaction":{"timestamp":"2026-03-11T14:58:35Z","km_from_current":18.86}}'
for i in $(seq 1 1000); do
  curl -s -o /dev/null -w "%{time_total}\n" -X POST http://localhost:9999/fraud-score \
    -H 'Content-Type: application/json' --data-raw "$PAYLOAD"
done | python3 -c "
import sys
xs = sorted(float(l)*1000 for l in sys.stdin)
n = len(xs)
print(f'p50={xs[n//2]:.3f}ms p95={xs[int(n*0.95)]:.3f}ms p99={xs[int(n*0.99)]:.3f}ms max={xs[-1]:.3f}ms')
"
docker stats --no-stream
docker compose down
```

## Formato sugerido do reporte final

Ao terminar o review, devolva ao usuário:

1. **Resumo** em 3-5 frases: o que foi auditado, o que está bem, o que está em risco.
2. **Achados críticos** (categoria + arquivo:linha + risco): coisas que devem ser corrigidas antes de submeter.
3. **Achados importantes** (categoria + sugestão + ganho estimado): melhorias acionáveis.
4. **Comparação com top performers**: tabela de "estamos fazendo X / top fazem Y".
5. **Métricas atuais**: quality_check + bench de latência.
6. **Próximos passos sugeridos**, ordenados por ratio impacto/esforço.

**Não invente números** — sempre cite valores observados nos comandos rodados. **Não sugira refator quando o código atual funciona** — só aponte com ganho mensurável. **Mencione explicitamente quando uma sugestão tem custo (memória, build time, complexidade)**.

## Referências externas (use WebFetch se necessário)

- [The Rust Performance Book](https://nnethercote.github.io/perf-book/) — referência principal
- [FAISS Wiki — Indexes](https://github.com/facebookresearch/faiss/wiki/Faiss-indexes) — IVF/HNSW/PQ
- [Pinecone — Composite Indexes](https://www.pinecone.io/learn/series/faiss/composite-indexes/) — IVF tuning
- [Qdrant — Quantization](https://qdrant.tech/documentation/manage-data/quantization/)
- [Nick Wilcox — target_cpu vs target_feature](https://www.nickwilcox.com/blog/target_cpu_vs_target_feature/)
- [Tokio docs — runtime](https://docs.rs/tokio/latest/tokio/runtime/index.html)

## Top performers Rust 2026 (pra comparação contextual)

| Owner | Repo | Stack chave | Notas |
|---|---|---|---|
| davidalecrim1 | rust-extreme | IVF K=1024 + 2-stage nprobe + row packing 16B + AVX2 manual + nginx/UDS + include_bytes | Possivelmente top |
| daniloitagyba | rinha-2026-rust | mmap + HAProxy/UDS + EXACT_FALLBACK approach | Único top com mmap (igual a nós) |
| MXLange | rust | brute force 100K + AVX2 madd_epi16 + nginx/UDS | p99=4.66ms / score=5331 reportados |
| lothyriel | rinha_2026 | IVF + axum + current_thread + nginx/UDS | Único top com current_thread (igual a nós) |
| jairoblatt | rinha-2026-rust | **mio puro** + brute force scalar | Sem framework async, padrão raro |

(Lista completa observada em `/Users/chagas42/@studies/rinha-de-backend-2026/participants/*.json`.)
