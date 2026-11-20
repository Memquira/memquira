//! The deadline of the request a task is serving (admission, used by provider retries).
//!
//! The API's admission layer runs every admitted request inside [`with_request_deadline`]; code
//! below it (provider permits, provider retry delays) reads [`current_deadline`] to avoid waiting
//! past it. Work outside a request (background jobs, refreshes) has no deadline here.

use std::future::Future;
use std::time::Instant;

tokio::task_local! {
    static REQUEST_DEADLINE: Instant;
}

/// Runs `future` with `deadline` as the current request deadline.
pub async fn with_request_deadline<F: Future>(deadline: Instant, future: F) -> F::Output {
    REQUEST_DEADLINE.scope(deadline, future).await
}

/// The deadline of the request this task is serving, if any.
pub fn current_deadline() -> Option<Instant> {
    REQUEST_DEADLINE.try_with(|deadline| *deadline).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn deadline_is_visible_inside_the_scope_only() {
        assert!(current_deadline().is_none());
        let deadline = Instant::now() + Duration::from_secs(5);
        let seen = with_request_deadline(deadline, async { current_deadline() }).await;
        assert_eq!(seen, Some(deadline));
        assert!(current_deadline().is_none());
    }
}
