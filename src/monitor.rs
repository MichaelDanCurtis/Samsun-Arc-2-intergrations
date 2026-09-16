//! Monitor backends and the chain that makes any of them dependable.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tracing::{info, warn};

use crate::input::Input;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("not authorized; run the backend's login again")]
    Unauthorized,
    #[error("http: {0}")]
    Http(String),
    #[error("unexpected response: {0}")]
    Protocol(String),
    #[error("{0}")]
    Unsupported(String),
}

#[async_trait]
pub trait MonitorBackend: Send + Sync {
    fn name(&self) -> &'static str;
    /// The input the monitor shows, or `None` when this backend cannot read.
    async fn current(&self) -> Result<Option<Input>, BackendError>;
    async fn switch(&self, input: Input) -> Result<(), BackendError>;
}

/// Every backend failed, twice.
#[derive(Debug, thiserror::Error)]
#[error("every monitor backend failed: {}", .attempts.iter().map(|(b, e)| format!("{b}: {e}")).collect::<Vec<_>>().join("; "))]
pub struct ChainError {
    pub attempts: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ensure {
    AlreadyThere,
    Switched { backend: &'static str },
}

pub struct Chain {
    backends: Vec<Arc<dyn MonitorBackend>>,
    op_timeout: Duration,
    retry_delay: Duration,
}

impl Chain {
    #[must_use]
    pub fn new(backends: Vec<Arc<dyn MonitorBackend>>) -> Self {
        Self {
            backends,
            op_timeout: Duration::from_secs(5),
            retry_delay: Duration::from_secs(3),
        }
    }

    #[must_use]
    pub fn with_timing(mut self, op_timeout: Duration, retry_delay: Duration) -> Self {
        self.op_timeout = op_timeout;
        self.retry_delay = retry_delay;
        self
    }

    /// The first backend that can read the input.
    pub async fn current(&self) -> Option<Input> {
        for backend in &self.backends {
            match tokio::time::timeout(self.op_timeout, backend.current()).await {
                Ok(Ok(Some(input))) => return Some(input),
                Ok(Ok(None)) => {}
                Ok(Err(error)) => warn!(backend = backend.name(), %error, "read failed"),
                Err(_) => warn!(backend = backend.name(), "read timed out"),
            }
        }
        None
    }

    /// Switch through the first backend that succeeds; one retry pass.
    pub async fn switch(&self, input: Input) -> Result<&'static str, ChainError> {
        let mut attempts = Vec::new();
        for pass in 0..2 {
            if pass == 1 {
                tokio::time::sleep(self.retry_delay).await;
            }
            for backend in &self.backends {
                match tokio::time::timeout(self.op_timeout, backend.switch(input)).await {
                    Ok(Ok(())) => {
                        info!(backend = backend.name(), %input, "monitor switched");
                        return Ok(backend.name());
                    }
                    Ok(Err(error)) => {
                        warn!(backend = backend.name(), %error, "switch failed");
                        attempts.push((backend.name().to_string(), error.to_string()));
                    }
                    Err(_) => {
                        warn!(backend = backend.name(), "switch timed out");
                        attempts.push((backend.name().to_string(), "timed out".to_string()));
                    }
                }
            }
        }
        Err(ChainError { attempts })
    }

    /// Read first; write only when the monitor is not already on `input`.
    pub async fn ensure(&self, input: Input) -> Result<Ensure, ChainError> {
        if self.current().await == Some(input) {
            return Ok(Ensure::AlreadyThere);
        }
        let backend = self.switch(input).await?;
        Ok(Ensure::Switched { backend })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct Fake {
        name: &'static str,
        current: Mutex<Option<Input>>,
        fail: bool,
        delay: Duration,
        switches: AtomicUsize,
    }

    impl Fake {
        fn new(name: &'static str) -> Arc<Self> {
            Arc::new(Self {
                name,
                current: Mutex::new(None),
                fail: false,
                delay: Duration::ZERO,
                switches: AtomicUsize::new(0),
            })
        }
        fn failing(name: &'static str) -> Arc<Self> {
            Arc::new(Self {
                fail: true,
                ..Self::plain(name)
            })
        }
        fn slow(name: &'static str, delay: Duration) -> Arc<Self> {
            Arc::new(Self {
                delay,
                ..Self::plain(name)
            })
        }
        fn plain(name: &'static str) -> Self {
            Self {
                name,
                current: Mutex::new(None),
                fail: false,
                delay: Duration::ZERO,
                switches: AtomicUsize::new(0),
            }
        }
        fn seen(name: &'static str, input: Input) -> Arc<Self> {
            let fake = Self::plain(name);
            *fake.current.lock().unwrap() = Some(input);
            Arc::new(fake)
        }
    }

    #[async_trait]
    impl MonitorBackend for Fake {
        fn name(&self) -> &'static str {
            self.name
        }
        async fn current(&self) -> Result<Option<Input>, BackendError> {
            Ok(*self.current.lock().unwrap())
        }
        async fn switch(&self, input: Input) -> Result<(), BackendError> {
            tokio::time::sleep(self.delay).await;
            self.switches.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(BackendError::Http("boom".into()));
            }
            *self.current.lock().unwrap() = Some(input);
            Ok(())
        }
    }

    fn chain(backends: Vec<Arc<dyn MonitorBackend>>) -> Chain {
        Chain::new(backends).with_timing(Duration::from_millis(50), Duration::from_millis(10))
    }

    #[tokio::test]
    async fn switch_uses_the_first_working_backend_in_order() {
        let a = Fake::failing("a");
        let b = Fake::new("b");
        let used = chain(vec![a.clone() as Arc<dyn MonitorBackend>, b.clone()])
            .switch(Input::Hdmi2)
            .await
            .unwrap();
        assert_eq!(used, "b");
        assert_eq!(a.switches.load(Ordering::SeqCst), 1);
        assert_eq!(b.switches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_slow_backend_times_out_and_the_next_one_runs() {
        let slow = Fake::slow("slow", Duration::from_secs(5));
        let fast = Fake::new("fast");
        let used = chain(vec![slow as Arc<dyn MonitorBackend>, fast])
            .switch(Input::Hdmi1)
            .await
            .unwrap();
        assert_eq!(used, "fast");
    }

    #[tokio::test]
    async fn all_failing_backends_get_a_second_pass_then_error() {
        let a = Fake::failing("a");
        let err = chain(vec![a.clone() as Arc<dyn MonitorBackend>])
            .switch(Input::Hdmi3)
            .await
            .unwrap_err();
        assert_eq!(a.switches.load(Ordering::SeqCst), 2, "one retry pass");
        assert_eq!(err.attempts.len(), 2);
        assert_eq!(err.attempts[0].0, "a");
    }

    #[tokio::test]
    async fn ensure_reads_before_writing() {
        let a = Fake::seen("a", Input::Hdmi2);
        assert_eq!(
            chain(vec![a.clone() as Arc<dyn MonitorBackend>])
                .ensure(Input::Hdmi2)
                .await
                .unwrap(),
            Ensure::AlreadyThere
        );
        assert_eq!(a.switches.load(Ordering::SeqCst), 0);
        assert_eq!(
            chain(vec![a.clone() as Arc<dyn MonitorBackend>])
                .ensure(Input::Dp)
                .await
                .unwrap(),
            Ensure::Switched { backend: "a" }
        );
        assert_eq!(a.switches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn current_is_the_first_backend_that_knows() {
        let blind = Fake::new("blind");
        let seen = Fake::seen("seen", Input::Hdmi1);
        assert_eq!(
            chain(vec![blind as Arc<dyn MonitorBackend>, seen])
                .current()
                .await,
            Some(Input::Hdmi1)
        );
    }
}
