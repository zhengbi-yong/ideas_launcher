//! 让一次查询停下来的凭据。
//!
//! 一次按键要扫全盘,而下一次按键往往在几百毫秒之后就来 ——
//! 这时候上一次的结果已经过时了,留着它只是白费力气。
//!
//! 这个凭据就是一个**代号**:新的一次查询把代号加一,还在跑的旧查询看到这个
//! ���数变了,就知道自己该停了。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 让一次查询停下来的凭据。
///
/// 克隆一份的是同一份代号,所以持有者之间可以互相通知。
#[derive(Clone, Default)]
pub struct Cancel {
    generation: Arc<AtomicU64>,
}

impl Cancel {
    /// 开一份新的凭据。
    pub fn new() -> Self {
        Cancel::default()
    }

    /// 开始一次新查询。
    ///
    /// 还在跑的旧查询看到这个数变了,就会停下来。
    pub fn bump(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// 现在的代号。
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// 这一次查询是不是已经过期了。
    pub fn is_stale(&self, generation: u64) -> bool {
        self.generation() != generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_query_makes_the_old_one_stale() {
        let cancel = Cancel::new();
        let first = cancel.generation();
        assert!(!cancel.is_stale(first));
        cancel.bump();
        assert!(cancel.is_stale(first));
        assert!(!cancel.is_stale(cancel.generation()));
    }

    #[test]
    fn a_copy_shares_the_generation() {
        let cancel = Cancel::new();
        let copy = cancel.clone();
        let first = copy.generation();
        cancel.bump();
        assert!(copy.is_stale(first));
    }
}
