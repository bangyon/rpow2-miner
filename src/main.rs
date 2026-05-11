use sha2::{Sha256, Digest};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Instant, Duration};
use std::fs;
use std::io::{self, Write};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::time::sleep;

const API_BASE: &str = "https://api.rpow2.com";
const RETRY_DELAYS: &[u64] = &[2, 5, 10, 30, 60]; // exponential backoff seconds

// ─── Structs ──────────────────────────────────────────────────────────────────

#[derive(Deserialize, Debug, Clone)]
struct Challenge {
    challenge_id: String,
    nonce_prefix: String,
    difficulty_bits: u32,
}

#[derive(Serialize)]
struct MintRequest {
    challenge_id: String,
    solution_nonce: String,
}

#[derive(Deserialize, Default)]
struct Me {
    email: Option<String>,
    balance: Option<u64>,
}

#[derive(Clone)]
struct AccountStats {
    label: String,
    minted: u64,
    errors: u64,
    total_hashes: u64,
    total_ms: u64,
    balance: u64,
    hashrate: f64, // MH/s
    start_time: Instant,
}

impl AccountStats {
    fn new(label: String) -> Self {
        Self {
            label,
            minted: 0,
            errors: 0,
            total_hashes: 0,
            total_ms: 0,
            balance: 0,
            hashrate: 0.0,
            start_time: Instant::now(),
        }
    }

    fn tokens_per_hour(&self) -> f64 {
        let elapsed_secs = self.start_time.elapsed().as_secs_f64();
        if elapsed_secs < 1.0 { return 0.0; }
        self.minted as f64 / elapsed_secs * 3600.0
    }

    fn avg_solve_ms(&self) -> u64 {
        if self.minted == 0 { return 0; }
        self.total_ms / self.minted
    }
}

// ─── Hex / Mining Utils ───────────────────────────────────────────────────────

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn trailing_zero_bits(hash: &[u8]) -> u32 {
    let mut count = 0u32;
    for i in (0..hash.len()).rev() {
        let byte = hash[i];
        if byte == 0 {
            count += 8;
        } else {
            count += byte.trailing_zeros();
            return count;
        }
    }
    count
}

fn mine_range(
    prefix: &[u8],
    difficulty: u32,
    start: u64,
    step: u64,
    found: &AtomicBool,
    hash_counter: &AtomicU64,
) -> Option<u64> {
    let mut buf = vec![0u8; prefix.len() + 8];
    buf[..prefix.len()].copy_from_slice(prefix);
    let mut nonce = start;
    let mut local_count = 0u64;

    loop {
        if found.load(Ordering::Relaxed) {
            hash_counter.fetch_add(local_count, Ordering::Relaxed);
            return None;
        }

        buf[prefix.len()..].copy_from_slice(&nonce.to_le_bytes());
        let hash = Sha256::digest(&buf);

        if trailing_zero_bits(&hash) >= difficulty {
            found.store(true, Ordering::Relaxed);
            hash_counter.fetch_add(local_count + 1, Ordering::Relaxed);
            return Some(nonce);
        }

        nonce = nonce.wrapping_add(step);
        local_count += 1;

        // Update counter setiap 10k hashes
        if local_count % 10_000 == 0 {
            hash_counter.fetch_add(10_000, Ordering::Relaxed);
            local_count = 0;
        }
    }
}

// ─── API Client ───────────────────────────────────────────────────────────────

async fn fetch_me(client: &Client, cookie: &str) -> Option<Me> {
    let r = client
        .get(format!("{}/me", API_BASE))
        .header("cookie", cookie)
        .send()
        .await
        .ok()?;
    r.json::<Me>().await.ok()
}

async fn fetch_challenge(client: &Client, cookie: &str) -> Result<Challenge, String> {
    let r = client
        .post(format!("{}/challenge", API_BASE))
        .header("cookie", cookie)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = r.status();
    if !status.is_success() {
        let body = r.text().await.unwrap_or_default();
        return Err(format!("HTTP {}: {}", status, body));
    }

    r.json::<Challenge>().await.map_err(|e| e.to_string())
}

async fn submit_mint(client: &Client, cookie: &str, req: MintRequest) -> Result<(), String> {
    let r = client
        .post(format!("{}/mint", API_BASE))
        .header("cookie", cookie)
        .json(&req)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = r.status();
    if status.is_success() {
        Ok(())
    } else {
        let body = r.text().await.unwrap_or_default();
        Err(format!("HTTP {}: {}", status, body))
    }
}

// ─── Dashboard ────────────────────────────────────────────────────────────────

fn print_dashboard(all_stats: &[AccountStats], global_start: &Instant) {
    let uptime = global_start.elapsed();
    let hh = uptime.as_secs() / 3600;
    let mm = (uptime.as_secs() % 3600) / 60;
    let ss = uptime.as_secs() % 60;

    let total_minted: u64 = all_stats.iter().map(|s| s.minted).sum();
    let total_errors: u64 = all_stats.iter().map(|s| s.errors).sum();
    let total_hashrate: f64 = all_stats.iter().map(|s| s.hashrate).sum();
    let total_balance: u64 = all_stats.iter().map(|s| s.balance).sum();

    println!("\n╔══════════════════════════════════════════════════════════╗");
    println!("║  rpow2 Enhanced Miner  │  Uptime: {:02}:{:02}:{:02}              ║", hh, mm, ss);
    println!("╠══════════════════════════════════════════════════════════╣");
    println!("║  Total Minted : {:<8}  Errors : {:<8}               ║", total_minted, total_errors);
    println!("║  Hashrate     : {:<8.2} MH/s  Balance: {:<8}         ║", total_hashrate, total_balance);
    println!("╠══════════════════════════════════════════════════════════╣");

    for s in all_stats {
        println!("║  [{:<20}]                              ║", &s.label[..s.label.len().min(20)]);
        println!("║    Minted: {:<6}  Errors: {:<6}  Bal: {:<8}     ║",
            s.minted, s.errors, s.balance);
        println!("║    Hashrate: {:<6.2} MH/s  Avg solve: {:<6}ms         ║",
            s.hashrate, s.avg_solve_ms());
        println!("║    Rate: {:.1} tokens/hr                               ║",
            s.tokens_per_hour());
    }

    println!("╚══════════════════════════════════════════════════════════╝");
    let _ = io::stdout().flush();
}

// ─── Mine Account ─────────────────────────────────────────────────────────────

async fn mine_account(
    cookie: String,
    threads: usize,
    account_idx: usize,
    stats_tx: tokio::sync::mpsc::Sender<(usize, AccountStats)>,
) {
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    // Get label dari /me
    let label = match fetch_me(&client, &cookie).await {
        Some(me) => me.email.unwrap_or_else(|| format!("account-{}", account_idx + 1)),
        None => format!("account-{}", account_idx + 1),
    };

    println!("[*] [{}] Starting with {} threads", label, threads);

    let mut stats = AccountStats::new(label.clone());
    let mut retry_count = 0usize;

    loop {
        // ── Fetch challenge ──
        let challenge = match fetch_challenge(&client, &cookie).await {
            Ok(c) => {
                retry_count = 0;
                c
            },
            Err(e) => {
                stats.errors += 1;
                let delay = RETRY_DELAYS[retry_count.min(RETRY_DELAYS.len() - 1)];
                eprintln!("[!] [{}] Challenge error (retry in {}s): {}", label, delay, e);
                let _ = stats_tx.send((account_idx, stats.clone())).await;
                sleep(Duration::from_secs(delay)).await;
                retry_count = (retry_count + 1).min(RETRY_DELAYS.len() - 1);
                continue;
            }
        };

        println!("[*] [{}] Challenge: {} | {} bits", label, challenge.challenge_id, challenge.difficulty_bits);

        let prefix = hex_to_bytes(&challenge.nonce_prefix);
        let difficulty = challenge.difficulty_bits;
        let found = Arc::new(AtomicBool::new(false));
        let solution = Arc::new(AtomicU64::new(0));
        let hash_counter = Arc::new(AtomicU64::new(0));

        let t0 = Instant::now();

        // ── Spawn mining threads ──
        let handles: Vec<_> = (0..threads)
            .map(|i| {
                let prefix = prefix.clone();
                let found = Arc::clone(&found);
                let solution = Arc::clone(&solution);
                let hash_counter = Arc::clone(&hash_counter);
                thread::spawn(move || {
                    if let Some(nonce) = mine_range(&prefix, difficulty, i as u64, threads as u64, &found, &hash_counter) {
                        solution.store(nonce, Ordering::Relaxed);
                    }
                })
            })
            .collect();

        for h in handles {
            let _ = h.join();
        }

        let elapsed_ms = t0.elapsed().as_millis() as u64;
        let hashes = hash_counter.load(Ordering::Relaxed);
        let nonce = solution.load(Ordering::Relaxed);
        let hashrate = if elapsed_ms > 0 {
            hashes as f64 / elapsed_ms as f64 / 1000.0
        } else { 0.0 };

        println!("[+] [{}] Found nonce: {} in {:.2}s ({:.2} MH/s)",
            label, nonce, elapsed_ms as f64 / 1000.0, hashrate);

        stats.total_hashes += hashes;
        stats.total_ms += elapsed_ms;
        stats.hashrate = hashrate;

        // ── Submit mint ──
        let mint_req = MintRequest {
            challenge_id: challenge.challenge_id,
            solution_nonce: nonce.to_string(),
        };

        match submit_mint(&client, &cookie, mint_req).await {
            Ok(_) => {
                stats.minted += 1;
                println!("[+] [{}] Minted! Total: {}", label, stats.minted);

                // Cek balance setiap 5 mint
                if stats.minted % 5 == 0 {
                    if let Some(me) = fetch_me(&client, &cookie).await {
                        stats.balance = me.balance.unwrap_or(stats.balance);
                        println!("[*] [{}] Balance: {}", label, stats.balance);
                    }
                }
            }
            Err(e) => {
                stats.errors += 1;
                eprintln!("[!] [{}] Mint failed: {}", label, e);

                // Detect session expired
                if e.contains("401") || e.contains("UNAUTHORIZED") || e.contains("auth") {
                    eprintln!("[!] [{}] Session expired! Update cookie in accounts.txt", label);
                    sleep(Duration::from_secs(60)).await;
                }
            }
        }

        let _ = stats_tx.send((account_idx, stats.clone())).await;
    }
}

// ─── Main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let accounts_file = std::env::var("RPOW_ACCOUNTS")
        .unwrap_or_else(|_| "accounts.txt".to_string());

    let total_threads: usize = std::env::var("RPOW_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(num_cpus::get);

    let dashboard_interval: u64 = std::env::var("RPOW_DASHBOARD_INTERVAL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);

    let accounts = load_accounts(&accounts_file);
    let num_accounts = accounts.len();

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║          rpow2 Enhanced Miner  v2.0                     ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!("[*] Accounts : {}", num_accounts);
    println!("[*] Threads  : {} total ({} per account)", total_threads, (total_threads / num_accounts).max(1));
    println!("[*] Dashboard: every {}s\n", dashboard_interval);

    let threads_per_account = (total_threads / num_accounts).max(1);
    let global_start = Instant::now();

    // Channel untuk stats dari setiap account
    let (stats_tx, mut stats_rx) = tokio::sync::mpsc::channel::<(usize, AccountStats)>(100);

    // Spawn account tasks
    let mut tasks = Vec::new();
    for (i, cookie) in accounts.into_iter().enumerate() {
        let tx = stats_tx.clone();
        let handle = tokio::spawn(mine_account(cookie, threads_per_account, i, tx));
        tasks.push(handle);
    }
    drop(stats_tx);

    // Stats collector + dashboard printer
    let mut all_stats: Vec<AccountStats> = (0..num_accounts)
        .map(|i| AccountStats::new(format!("account-{}", i + 1)))
        .collect();

    let mut last_dashboard = Instant::now();

    tokio::spawn(async move {
        while let Some((idx, stats)) = stats_rx.recv().await {
            if idx < all_stats.len() {
                all_stats[idx] = stats;
            }
            if last_dashboard.elapsed().as_secs() >= dashboard_interval {
                print_dashboard(&all_stats, &global_start);
                last_dashboard = Instant::now();
            }
        }
    });

    for task in tasks {
        let _ = task.await;
    }
}

fn load_accounts(path: &str) -> Vec<String> {
    let content = fs::read_to_string(path).unwrap_or_else(|_| {
        eprintln!("[!] Cannot read {}. Create it with one cookie per line.", path);
        std::process::exit(1);
    });
    content
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}
