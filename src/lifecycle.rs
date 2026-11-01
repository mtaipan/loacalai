//! Optional user deadlines. Zero means unlimited; an operation timeout is not a task lifetime.
use anyhow::Result;
use std::{future::Future, time::Duration};
use tokio::time::Instant;

pub fn deadline(seconds: u64) -> Option<Instant> {
    (seconds != 0).then(|| Instant::now() + Duration::from_secs(seconds))
}
pub async fn wait(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending::<()>().await,
    }
}
pub async fn bounded<T>(
    deadline: Option<Instant>,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! {
        result=work=>result,
        _=wait(deadline)=>anyhow::bail!("Истёк заданный пользователем срок операции"),
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test(start_paused = true)]
    async fn no_implicit_deadline_and_explicit_deadline_still_works() {
        let work = async {
            tokio::time::sleep(std::time::Duration::from_secs(7201)).await;
            Ok(7)
        };
        assert_eq!(super::bounded(super::deadline(0), work).await.unwrap(), 7);
        let work = async {
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
            Ok(7)
        };
        assert!(super::bounded(super::deadline(10), work).await.is_err());
    }
}
