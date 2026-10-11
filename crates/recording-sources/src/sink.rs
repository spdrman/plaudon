//! The byte sink a device writes an acquisition into.

use async_trait::async_trait;

/// Failure writing to a staging sink.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SinkError {
    /// The underlying storage rejected the write.
    #[error("staging write failed")]
    WriteFailed,
    /// The sink's configured byte budget was exceeded.
    ///
    /// A device-reported length is untrusted, so the sink caps allocation
    /// independently (spec 7.7).
    #[error("staging write exceeded the configured byte budget")]
    BudgetExceeded,
}

/// A bounded, append-only staging sink.
#[async_trait]
pub trait AsyncWriteSink: Send {
    /// Append bytes.
    async fn write_all(&mut self, bytes: &[u8]) -> Result<(), SinkError>;

    /// Flush buffered bytes to durable storage.
    async fn flush(&mut self) -> Result<(), SinkError>;

    /// Bytes accepted so far.
    fn written(&self) -> u64;
}

/// An in-memory sink with an explicit byte budget, for tests and fakes.
#[derive(Debug)]
pub struct InMemorySink {
    buffer: Vec<u8>,
    budget: u64,
    flushed: u64,
}

impl InMemorySink {
    /// Create a sink that refuses to accept more than `budget` bytes.
    #[must_use]
    pub const fn with_budget(budget: u64) -> Self {
        Self {
            buffer: Vec::new(),
            budget,
            flushed: 0,
        }
    }

    /// The accepted bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.buffer
    }

    /// Bytes that have been flushed.
    #[must_use]
    pub const fn flushed(&self) -> u64 {
        self.flushed
    }
}

#[async_trait]
impl AsyncWriteSink for InMemorySink {
    async fn write_all(&mut self, bytes: &[u8]) -> Result<(), SinkError> {
        if self.buffer.len() as u64 + bytes.len() as u64 > self.budget {
            return Err(SinkError::BudgetExceeded);
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), SinkError> {
        self.flushed = self.buffer.len() as u64;
        Ok(())
    }

    fn written(&self) -> u64 {
        self.buffer.len() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn a_sink_refuses_to_exceed_its_budget() {
        let mut sink = InMemorySink::with_budget(4);
        sink.write_all(b"abcd").await.unwrap();
        assert_eq!(sink.write_all(b"e").await, Err(SinkError::BudgetExceeded));
        assert_eq!(sink.written(), 4);
    }
}
