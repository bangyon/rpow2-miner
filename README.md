# rpow2-miner

Multi-account Rust miner for [rpow2.com](https://rpow2.com).

## Setup

```bash
cargo build --release
```

## Usage

1. Copy `accounts.txt.example` to `accounts.txt`
2. Isi dengan session cookie tiap akun (satu per baris)
3. Jalankan:

```bash
./target/release/rpow2-miner
```

## Config

| Env var | Default | Keterangan |
|---|---|---|
| `RPOW_ACCOUNTS` | `accounts.txt` | Path ke file akun |
| `RPOW_THREADS` | jumlah CPU | Total threads (dibagi rata per akun) |

## Cara dapat cookie

1. Login di https://rpow2.com
2. DevTools (F12) → Application → Cookies → `api.rpow2.com`
3. Copy value `rpow_session`
