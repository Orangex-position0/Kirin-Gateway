#![allow(dead_code)]

use crate::control_plane::admin_api::dto::RateLimitDTO;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use tracing::warn;

/// 限流功能
pub struct RateLimiter {
    /// 是否启用限流
    enabled: bool,
    /// 令牌桶容量
    capacity: AtomicUsize,
    /// 每秒补充速率
    refill_rate: AtomicUsize,
    /// 令牌桶
    /// - key : IP
    /// - value : token bucket
    buckets: Mutex<HashMap<String, TokenBucket>>,
}

impl RateLimiter {
    pub fn new(capacity: usize, refill_rate: usize) -> Self {
        RateLimiter {
            enabled: true,
            capacity: AtomicUsize::new(capacity),
            refill_rate: AtomicUsize::new(refill_rate),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// 检查某个 IP 是否被允许通过
    /// return: (是否允许通过, 剩余令牌数)
    pub fn check(&self, ip: &str) -> (bool, usize) {
        let mut buckets = self.buckets.lock().unwrap();
        let bucket = buckets.entry(ip.to_string()).or_insert_with(|| {
            TokenBucket::new(
                self.capacity.load(Ordering::Relaxed),
                self.refill_rate.load(Ordering::Relaxed),
            )
        });

        let allowed = bucket.try_acquire();
        let remaining = bucket.remaining();
        (allowed, remaining)
    }

    /// 更新限流策略的参数
    pub fn update_policy(&self, capacity: usize, refill_rate: usize) {
        self.capacity.store(capacity, Ordering::Relaxed);
        self.refill_rate.store(refill_rate, Ordering::Relaxed);
        warn!(
            "限流策略已更新: capacity={}, refill_rate={}/s",
            capacity, refill_rate
        );
    }

    /// 获取当前限流策略概览
    pub fn summary(&self) -> RateLimitDTO {
        RateLimitDTO {
            enabled: self.enabled,
            capacity: Some(self.capacity.load(Ordering::Relaxed)),
            refill_rate: Some(self.refill_rate.load(Ordering::Relaxed)),
        }
    }
}

/// token bucket
pub struct TokenBucket {
    capacity: usize,       // 最大 token 数
    current_tokens: usize, // 当前 token 数
    refill_rate: usize,    // token 每秒补充速率
    last_refill: Instant,  // 上次补充时间
}

impl TokenBucket {
    pub fn new(capacity: usize, refill_rate: usize) -> Self {
        TokenBucket {
            capacity,
            current_tokens: capacity,
            refill_rate,
            last_refill: Instant::now(),
        }
    }

    /// try to acquire a token from token bucket
    pub fn try_acquire(&mut self) -> bool {
        self.refill();
        let (allowed, remaining) = try_acquire_decision(self.current_tokens);
        self.current_tokens = remaining;
        allowed
    }

    /// acquire remaining tokens in token bucket
    pub fn remaining(&self) -> usize {
        self.current_tokens
    }

    /// refill tokens by time elapse
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs();
        if elapsed > 0 {
            self.current_tokens = calculate_refill_tokens(
                elapsed,
                self.current_tokens,
                self.refill_rate,
                self.capacity,
            );
            self.last_refill = now;
        }
    }
}

/// pure function: 计算补充后的令牌数
///
/// - input：经过秒数、当前令牌数、补充速率、容量上限
/// - output：补充后的令牌数（不超过容量）
fn calculate_refill_tokens(
    elapsed_secs: u64,
    current_tokens: usize,
    refill_rate: usize,
    capacity: usize,
) -> usize {
    let tokens_to_add = (elapsed_secs as usize) * refill_rate;
    (current_tokens + tokens_to_add).min(capacity)
}

/// pure function: 判断是否允许请求，并返回消耗后的令牌数
///
/// - input: 当前令牌数
/// - output: (是否允许请求, 消耗后的令牌数)
fn try_acquire_decision(current_tokens: usize) -> (bool, usize) {
    if current_tokens > 0 {
        (true, current_tokens - 1)
    } else {
        (false, current_tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    /// TokenBucket：令牌耗尽后 try_acquire 返回 false
    #[test]
    fn test_bucket_exhaust_then_reject() {
        let mut bucket = TokenBucket::new(2, 1);
        assert!(bucket.try_acquire());
        assert!(bucket.try_acquire());
        assert!(!bucket.try_acquire());
    }

    /// TokenBucket：remaining 正确反映剩余令牌数
    #[test]
    fn test_bucket_remaining() {
        let mut bucket = TokenBucket::new(5, 1);
        assert_eq!(bucket.remaining(), 5);
        bucket.try_acquire();
        bucket.try_acquire();
        assert_eq!(bucket.remaining(), 3);
    }

    /// TokenBucket：等待 1 秒后令牌补充
    #[test]
    fn test_bucket_refill_after_wait() {
        let mut bucket = TokenBucket::new(5, 2);
        // 消耗所有令牌
        for _ in 0..5 {
            assert!(bucket.try_acquire());
        }
        assert_eq!(bucket.remaining(), 0);
        assert!(!bucket.try_acquire());

        // 等待 1 秒，应补充 2 个令牌
        thread::sleep(Duration::from_secs(1));
        assert!(bucket.try_acquire());
        assert_eq!(bucket.remaining(), 1);
    }

    /// TokenBucket：补充不超过容量上限
    #[test]
    fn test_bucket_refill_capped_at_capacity() {
        let bucket = TokenBucket::new(3, 10);
        // 不消耗令牌，等待 1 秒
        thread::sleep(Duration::from_secs(1));
        // remaining 不应超过 capacity
        assert!(bucket.remaining() <= 3);
    }

    /// RateLimiter：不同 IP 拥有独立的令牌桶
    #[test]
    fn test_limiter_ip_isolation() {
        let limiter = RateLimiter::new(2, 1);

        // IP1 消耗所有令牌
        let (allowed1, remaining1) = limiter.check("192.168.1.1");
        assert!(allowed1);
        assert_eq!(remaining1, 1);

        let (allowed2, remaining2) = limiter.check("192.168.1.1");
        assert!(allowed2);
        assert_eq!(remaining2, 0);

        // IP1 的令牌耗尽
        let (allowed3, _) = limiter.check("192.168.1.1");
        assert!(!allowed3);

        // IP2 有独立的令牌桶，应该正常
        let (allowed4, remaining4) = limiter.check("192.168.1.2");
        assert!(allowed4);
        assert_eq!(remaining4, 1);
    }

    /// RateLimiter：策略动态更新影响新创建的令牌桶
    #[test]
    fn test_limiter_policy_update() {
        let limiter = RateLimiter::new(2, 1);

        // 更新策略：容量改为 5
        limiter.update_policy(5, 1);

        // 新 IP 应使用新的 capacity=5
        let mut remaining_check = 5;
        for _ in 0..5 {
            let (allowed, remaining) = limiter.check("10.0.0.1");
            assert!(allowed);
            remaining_check = remaining;
        }
        assert_eq!(remaining_check, 0);

        // 第 6 次应该被拒绝
        let (allowed, _) = limiter.check("10.0.0.1");
        assert!(!allowed);
    }

    /// Pure Function Test
    #[test]
    fn test_calculate_refill_tokens_no_elapsed() {
        // elapsed = 0，令牌数不变
        assert_eq!(calculate_refill_tokens(0, 10, 5, 100), 10);
    }

    #[test]
    fn test_calculate_refill_tokens_normal() {
        // elapsed = 3, refill_rate = 5, current = 10 → 10 + 15 = 25
        assert_eq!(calculate_refill_tokens(3, 10, 5, 100), 25);
    }

    #[test]
    fn test_calculate_refill_tokens_capped_at_capacity() {
        // elapsed = 1, refill_rate = 100, current = 95 → min(195, 100) = 100
        assert_eq!(calculate_refill_tokens(1, 95, 100, 100), 100);
    }

    #[test]
    fn test_try_acquire_decision_allowed() {
        assert_eq!(try_acquire_decision(5), (true, 4));
        assert_eq!(try_acquire_decision(1), (true, 0));
    }

    #[test]
    fn test_try_acquire_decision_rejected() {
        assert_eq!(try_acquire_decision(0), (false, 0));
    }
}
