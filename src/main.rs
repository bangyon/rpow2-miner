use sha2::{Sha256, Digest};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use std::fs;

use reqwest::Client;
use serde::{Deserialize, Serialize};

const API_BASE: &str = "https://api.rpow2.com";

#[derive(Deserialize, Debug)]
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

fn mine_range(prefix: &[u8], difficulty: u32, start: u64, step: u64, found: &AtomicBool) -> Option<u64> {
    let mut buf = vec![0u8; prefix.len() + 8];
    buf[..prefix.len()].copy_from_slice(prefix);
    let mut nonce = start;

    loop {
        if found.load(Ordering::Relaxed) {
            return None;
        }
        buf[prefix.len()..].copy_from_slice(&nonce.to_le_bytes());

        let hash = Sha256::digest(&buf);
        if trailing_zero_bits(&hash) >= difficulty {
            found.store(true, Ordering::Relaxed);
            return Some(nonce);
        }
        nonce = nonce.wrapping_add(step);
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

async fn mine_account(cookie: String, threads: usize, account_idx: usize) {
    let client = Client::builder().build().unwrap();
    let mut total_mined = 0u64;

    let label = if cookie.contains('@') {
        cookie.split('|').next().unwrap_or("???").to_string()
    } else {
        format!("account-{}", account_idx + 1)
    };

    println!("[*] [{}] Starting miner with {} threads", label, threads);

    loop {
        let res = client
            .post(format!("{}/challenge", API_BASE))
            .header("cookie", &cookie)
            .send()
            .await;

        let challenge: Challenge = match res {
            Ok(r) => match r.json().await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[!] [{}] Failed to parse challenge: {}", label, e);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            },
            Err(e) => {
                eprintln!("[!] [{}] Failed to fetch challenge: {}", label, e);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        println!("[*] [{}] Challenge: {} | Difficulty: {} bits",
            label, challenge.challenge_id, challenge.difficulty_bits);

        let prefix = hex_to_bytes(&challenge.nonce_prefix);
        let difficulty = challenge.difficulty_bits;
        let found = Arc::new(AtomicBool::new(false));
        let solution = Arc::new(AtomicU64::new(0));

        let start = Instant::now();

        let handles: Vec<_> = (0..threads)
            .map(|i| {
                let prefix = prefix.clone();
                let found = Arc::clone(&found);
                let solution = Arc::clone(&solution);
                thread::spawn(move || {
                    if let Some(nonce) = mine_range(&prefix, difficulty, i as u64, threads as u64, &found) {
                        solution.store(nonce, Ordering::Relaxed);
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        let elapsed = start.elapsed();
        let nonce = solution.load(Ordering::Relaxed);
        println!("[+] [{}] Found nonce: {} in {:.2}s", label, nonce, elapsed.as_secs_f64());

        let mint_req = MintRequest {
            challenge_id: challenge.challenge_id,
            solution_nonce: nonce.to_string(),
        };

        match client
            .post(format!("{}/mint", API_BASE))
            .header("cookie", &cookie)
            .json(&mint_req)
            .send()
            .await
        {
            Ok(r) => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                if status.is_success() {
                    total_mined += 1;
                    println!("[+] [{}] Minted! Total: {} | {}", label, total_mined, body);
                } else {
                    eprintln!("[!] [{}] Mint failed ({}): {}", label, status, body);
                }
            }
            Err(e) => eprintln!("[!] [{}] Mint request error: {}", label, e),
        }
    }
}

#[tokio::main]
async fn main() {
    let accounts_file = std::env::var("RPOW_ACCOUNTS")
        .unwrap_or_else(|_| "accounts.txt".to_string());

    let total_threads: usize = std::env::var("RPOW_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(num_cpus::get);

    let accounts = load_accounts(&accounts_file);
    let num_accounts = accounts.len();

    println!("[*] rpow2 Rust miner (multi-account)");
    println!("[*] Accounts: {}", num_accounts);
    println!("[*] Total threads: {}", total_threads);

    let threads_per_account = (total_threads / num_accounts).max(1);

    let mut tasks = Vec::new();
    for (i, cookie) in accounts.into_iter().enumerate() {
        let handle = tokio::spawn(mine_account(cookie, threads_per_account, i));
        tasks.push(handle);
    }

    for task in tasks {
        let _ = task.await;
    }
}
