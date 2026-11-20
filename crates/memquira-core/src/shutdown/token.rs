use tokio::sync::watch;

#[derive(Debug, Clone)]
pub struct ShutdownToken {
    sender: watch::Sender<bool>,
    receiver: watch::Receiver<bool>,
}

impl Default for ShutdownToken {
    fn default() -> Self {
        Self::new()
    }
}

impl ShutdownToken {
    pub fn new() -> Self {
        let (sender, receiver) = watch::channel(false);
        Self { sender, receiver }
    }

    pub fn cancel(&self) {
        let _ = self.sender.send(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }

        let mut receiver = self.receiver.clone();
        loop {
            if *receiver.borrow_and_update() {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }

    pub fn child_token(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            receiver: self.receiver.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn token_starts_not_cancelled() {
        let token = ShutdownToken::new();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn cancel_marks_token_cancelled() {
        let token = ShutdownToken::new();
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn cancelled_future_resolves_after_cancel() {
        let token = ShutdownToken::new();
        let child = token.child_token();

        let waiter = tokio::spawn(async move {
            child.cancelled().await;
        });
        tokio::time::sleep(Duration::from_millis(5)).await;
        token.cancel();

        tokio::time::timeout(Duration::from_millis(100), waiter)
            .await
            .expect("cancelled future should resolve")
            .expect("waiter task should complete");
    }

    #[tokio::test]
    async fn child_token_is_cancelled_when_parent_cancels() {
        let parent = ShutdownToken::new();
        let child = parent.child_token();

        parent.cancel();

        assert!(child.is_cancelled());
        tokio::time::timeout(Duration::from_millis(100), child.cancelled())
            .await
            .expect("child cancellation should resolve");
    }
}
