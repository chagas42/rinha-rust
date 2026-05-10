# rinha 2026

submissao em rust pra rinha de backend 2026 (fraude por busca vetorial)

stack: rust + hyper + haproxy via unix sockets, ivf k=1024 com quantizacao i16 e avx2 manual, roda em 1 cpu / 350mb

## rodar

precisa do `references.json.gz` no diretorio (gitignored, baixa do organizer)

```
cargo build --release
./target/release/preprocess references.json.gz index.bin 1024 10
INDEX_PATH=./index.bin ./target/release/rinha-2026
```

ou docker:
```
docker compose up -d
curl localhost:9999/ready
```

## testes

```
cargo test --lib
./target/release/quality_check index.bin test-data.json 32
```
