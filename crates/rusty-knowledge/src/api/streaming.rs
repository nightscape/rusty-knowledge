use super::types::ApiError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{pin::Pin, sync::Arc};
use tokio_stream::Stream;
use tokio::sync::{mpsc, Mutex};
/// Real-time change notification and state synchronization.
///
/// This trait provides race-free state sync by streaming the current document state
/// followed by all subsequent changes. Backends that support real-time updates implement this trait.
///
/// # Architecture
///
/// This trait uses vendor-neutral Rust async Streams (`tokio_stream::Stream`)
/// which can be adapted to any frontend technology:
/// - Flutter: Adapted via `StreamSink` in FRB bridge layer
/// - Tauri: Adapted via event emission in command layer
/// - REST/Web: Adapted via Server-Sent Events or WebSocket
///
/// # Example
///
/// ```rust,no_run
/// use rusty_knowledge::api::ChangeNotifications;
/// use tokio_stream::StreamExt;
///
/// async fn example(repo: impl ChangeNotifications<Block>) -> anyhow::Result<()> {
///     // Start watching - first receives all current blocks as Created events,
///     // then streams subsequent changes
///     let mut stream = repo.watch_changes_since(StreamPosition::Beginning).await;
///
///     // Process batched changes as they arrive
///     while let Some(result) = stream.next().await {
///         match result {
///             Ok(changes) => {
///                 for change in changes {
///                     println!("Block changed: {:?}", change);
///                 }
///             }
///             Err(e) => eprintln!("Change stream error: {:?}", e),
///         }
///     }
///
///     // Stream automatically unsubscribes when dropped
///     Ok(())
/// }
/// ```
#[async_trait]
pub trait ChangeNotifications<T>: Send + Sync {
    /// Subscribe to document changes since a specific position.
    ///
    /// Returns a Stream that emits batched document changes. Behavior depends on the `position` parameter:
    /// - `StreamPosition::Beginning`: First emits all current blocks as `Change::Created` events in a batch,
    ///   then continues streaming subsequent changes (initial sync mode)
    /// - `StreamPosition::Version(v)`: Streams only changes that occurred after version `v`
    ///   (incremental sync mode)
    ///
    /// # Arguments
    ///
    /// * `position` - Stream position to start from (beginning or specific version)
    ///
    /// # Returns
    ///
    /// A Stream that yields `Result<Vec<Change<T>>, ApiError>` items. Each item is a batch of changes.
    /// The stream continues until either:
    /// - It is explicitly dropped (automatic unsubscription)
    /// - An error occurs (yielded as `Err`)
    /// - The backend shuts down (stream closes)
    ///
    /// # Error Propagation
    ///
    /// Errors are propagated through the stream's Result type rather than
    /// terminating the stream. Backends may choose to:
    /// - Continue streaming after recoverable errors
    /// - Close the stream after fatal errors
    ///
    /// # Resource Management
    ///
    /// The stream automatically unsubscribes and releases resources when dropped.
    /// No explicit cleanup method needed.
    async fn watch_changes_since(
        &self,
        position: StreamPosition,
    ) -> Pin<Box<dyn Stream<Item = Result<Vec<Change<T>>, ApiError>> + Send>>;

    /// Get the current version vector of the document.
    ///
    /// Returns the version vector representing the current state of the document.
    /// This can be used to track document evolution over time.
    ///
    /// # Returns
    ///
    /// A version vector as a byte array.
    async fn get_current_version(&self) -> Result<Vec<u8>, ApiError>;
}


/// Type alias for change notification subscribers
pub type ChangeSubscribers<T> = Arc<Mutex<Vec<mpsc::Sender<Result<Vec<Change<T>>, ApiError>>>>>;

/// Position in the change stream to start watching from.
///
/// Used with `watch_changes_since()` to control whether to receive current state or only new changes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum StreamPosition {
    /// Start from the beginning: first stream all current blocks as Created events,
    /// then stream subsequent changes
    Beginning,
    /// Start from a specific version: stream only changes that occurred after this version
    Version(Vec<u8>),
}

/// Origin of a change event (local vs. remote).
///
/// Used to prevent UI echo when local changes sync back via P2P.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChangeOrigin {
    /// Change initiated by this client
    Local,
    /// Change received from P2P sync
    Remote,
}

/// Change notification event.
///
/// Emitted by the change stream to notify frontends of document updates.
/// Includes origin tracking to suppress echo of local edits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Change<T> {
    /// Block was created
    Created { data: T, origin: ChangeOrigin },
    /// Block content was updated (character-level)
    Updated {
        id: String,
        data: T,
        origin: ChangeOrigin,
    },
    /// Block was deleted (tombstone set)
    Deleted { id: String, origin: ChangeOrigin },
}
