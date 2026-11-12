//! UDP 网络探测：RTT / 抖动 / 丢包。
//!
//! 注意：这测的是**网络往返**，不是玻璃到玻璃延迟；玻璃到玻璃需要相机拍两个屏幕
//! 或共享时钟，见 `docs/low-latency.md`。

use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

#[derive(Serialize, Clone, Debug)]
pub struct ProbeStats {
    pub sent: u64,
    pub received: u64,
    pub lost: u64,
    pub loss_pct: f64,
    pub min_ms: f64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    pub jitter_ms: f64,
}

/// 阻塞式回显服务：把收到的每个 UDP 包原样发回。
pub async fn listen(bind: &str) -> Result<()> {
    let socket = UdpSocket::bind(bind)
        .await
        .with_context(|| format!("绑定 {bind}"))?;
    println!("probe listener on udp://{}", socket.local_addr()?);
    let mut buf = [0u8; 128];
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((len, addr)) => {
                let _ = socket.send_to(&buf[..len], addr).await;
            }
            Err(err) => eprintln!("probe recv error: {err}"),
        }
    }
}

pub async fn measure(peer: &str, count: u64, interval_ms: u64) -> Result<ProbeStats> {
    let addr: SocketAddr = peer
        .parse()
        .with_context(|| format!("peer 需为 ip:port：'{peer}'"))?;
    measure_addr(addr, count, interval_ms).await
}

pub async fn measure_addr(peer: SocketAddr, count: u64, interval_ms: u64) -> Result<ProbeStats> {
    if count == 0 {
        bail!("count 必须大于 0");
    }
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let mut buf = [0u8; 128];
    let mut rtts: Vec<f64> = Vec::new();

    for seq in 0..count {
        let payload = format!("LANV{:016x}", seq);
        let started = Instant::now();
        socket.send_to(payload.as_bytes(), peer).await?;
        match tokio::time::timeout(Duration::from_millis(500), socket.recv_from(&mut buf)).await {
            Ok(Ok(_)) => rtts.push(started.elapsed().as_secs_f64() * 1000.0),
            _ => {}
        }
        if interval_ms > 0 {
            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
        }
    }

    Ok(build_stats(count, &rtts))
}

fn build_stats(sent: u64, rtts: &[f64]) -> ProbeStats {
    let received = rtts.len() as u64;
    let lost = sent.saturating_sub(received);
    let mut sorted = rtts.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |p: f64| -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
        sorted[idx]
    };
    let mean = if sorted.is_empty() {
        0.0
    } else {
        sorted.iter().sum::<f64>() / sorted.len() as f64
    };
    let jitter = if rtts.len() < 2 {
        0.0
    } else {
        rtts.windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .sum::<f64>()
            / (rtts.len() - 1) as f64
    };
    ProbeStats {
        sent,
        received,
        lost,
        loss_pct: if sent == 0 {
            0.0
        } else {
            lost as f64 * 100.0 / sent as f64
        },
        min_ms: sorted.first().copied().unwrap_or(0.0),
        mean_ms: mean,
        p50_ms: pct(0.50),
        p95_ms: pct(0.95),
        max_ms: sorted.last().copied().unwrap_or(0.0),
        jitter_ms: jitter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_percentiles() {
        let stats = build_stats(10, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]);
        assert_eq!(stats.received, 10);
        assert_eq!(stats.lost, 0);
        assert!((stats.min_ms - 1.0).abs() < 1e-9);
        assert!((stats.max_ms - 10.0).abs() < 1e-9);
        assert!(stats.p50_ms >= 5.0 && stats.p50_ms <= 6.0);
        assert!(stats.p95_ms >= 9.0);
    }

    #[tokio::test]
    async fn loopback_echo() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 128];
            loop {
                if let Ok((len, peer)) = server.recv_from(&mut buf).await {
                    let _ = server.send_to(&buf[..len], peer).await;
                }
            }
        });
        let stats = measure_addr(addr, 20, 0).await.unwrap();
        assert_eq!(stats.received, 20, "loopback 不应丢包: {stats:?}");
        assert!(stats.p50_ms >= 0.0);
    }
}
