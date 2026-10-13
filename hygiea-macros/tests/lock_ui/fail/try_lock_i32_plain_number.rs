// try_lock_i32 的 key1 只接受注册过的锁类型
use hygiea::lock::DistributedLock;

async fn f(lock: &impl DistributedLock) {
    let _ = lock.try_lock_i32(1, 2).await;
}

fn main() {}
