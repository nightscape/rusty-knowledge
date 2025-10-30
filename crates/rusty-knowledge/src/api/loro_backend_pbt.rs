//! Stateful property-based tests comparing two DocumentRepository implementations
//!
//! This module tests LoroBackend against MemoryBackend by running identical
//! operations on both and comparing their results structurally.

#[cfg(test)]
mod stateful_tests {
    use super::super::loro_backend::LoroBackend;
    use super::super::memory_backend::MemoryBackend;
    use super::super::pbt_infrastructure::{
        BlockTransition, apply_transition, check_transition_preconditions,
        generate_crud_transitions, translate_transition, update_id_map_after_create,
        verify_backends_match,
    };
    use super::super::repository::{ChangeNotifications, CoreOperations, Lifecycle};
    use super::super::types::BlockChange;
    use crate::api::{StreamPosition, Traversal};
    use proptest::prelude::*;
    use proptest_state_machine::{ReferenceStateMachine, StateMachineTest};
    use std::collections::HashMap;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use tokio_stream::StreamExt;

    type WatcherId = usize;

    /// Stream wrapper for a watcher subscription
    struct WatcherStream {
        stream: Pin<
            Box<
                dyn futures::Stream<Item = Result<BlockChange, super::super::types::ApiError>>
                    + Send,
            >,
        >,
    }

    impl std::fmt::Debug for WatcherStream {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("WatcherStream").finish()
        }
    }

    /// Pure metadata for a watcher (cloneable)
    #[derive(Debug, Clone)]
    struct WatcherDescriptor {
        watcher_id: WatcherId,
        base_version_idx: usize,
        last_consumed_idx: usize,
        pending_events: Vec<BlockChange>,
    }

    /// Reference state wraps MemoryBackend (our reference implementation)
    #[derive(Debug)]
    struct ReferenceState {
        backend: MemoryBackend,
        /// Snapshot versions after each command
        versions: Vec<Vec<u8>>,
        /// Watcher metadata (cloneable data only)
        watchers: HashMap<WatcherId, WatcherDescriptor>,
        /// Live watcher streams (not cloned directly, rebuilt on clone)
        live_streams: HashMap<WatcherId, WatcherStream>,
        /// Runtime for async operations (wrapped in Arc to keep it alive)
        _runtime: Arc<tokio::runtime::Runtime>,
    }

    impl Default for ReferenceState {
        fn default() -> Self {
            let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());

            let backend = runtime
                .block_on(MemoryBackend::create_new("reference".to_string()))
                .unwrap();

            let initial_version = runtime.block_on(backend.get_current_version()).unwrap();

            Self {
                backend,
                versions: vec![initial_version],
                watchers: HashMap::new(),
                live_streams: HashMap::new(),
                _runtime: runtime,
            }
        }
    }

    impl Clone for ReferenceState {
        fn clone(&self) -> Self {
            let backend_clone = self.backend.clone();
            let versions_clone = self.versions.clone();
            let watchers_clone = self.watchers.clone();

            // Don't rebuild streams in Clone - do it lazily in apply() instead
            // This keeps Clone cheap and avoids async work during cloning
            let live_streams_clone = HashMap::new();

            Self {
                backend: backend_clone,
                versions: versions_clone,
                watchers: watchers_clone,
                live_streams: live_streams_clone,
                _runtime: self._runtime.clone(),
            }
        }
    }

    impl ReferenceStateMachine for ReferenceState {
        type State = Self;
        type Transition = BlockTransition;

        fn init_state() -> BoxedStrategy<Self::State> {
            Just(ReferenceState::default()).boxed()
        }

        fn transitions(state: &Self::State) -> BoxedStrategy<Self::Transition> {
            // Get all blocks including root (root will be parent for top-level user blocks)
            let all_blocks = state
                ._runtime
                .block_on(state.backend.get_all_blocks(Traversal::ALL))
                .expect("Failed to get all blocks in transitions()");
            let all_ids: Vec<String> = all_blocks.iter().map(|b| b.id.clone()).collect();
            let non_root_ids: Vec<String> = all_ids.iter().skip(1).cloned().collect();

            // Generate CRUD transitions using shared logic
            let crud_transitions = generate_crud_transitions(all_ids, non_root_ids);

            // Generate next available watcher ID
            let next_watcher_id = state.watchers.keys().max().map(|id| id + 1).unwrap_or(0);
            let watch_changes = Just(BlockTransition::WatchChanges {
                watcher_id: next_watcher_id,
            })
            .boxed();

            prop::strategy::Union::new(vec![crud_transitions, watch_changes]).boxed()
        }

        fn preconditions(state: &Self::State, transition: &Self::Transition) -> bool {
            // Handle watcher-specific preconditions first
            match transition {
                BlockTransition::WatchChanges { watcher_id } => {
                    !state.watchers.contains_key(watcher_id)
                }
                BlockTransition::UnwatchChanges { watcher_id } => {
                    state.watchers.contains_key(watcher_id)
                }
                _ => {
                    // Use shared async precondition checker for CRUD operations
                    state
                        ._runtime
                        .block_on(check_transition_preconditions(transition, &state.backend))
                }
            }
        }

        fn apply(state: Self::State, transition: &Self::Transition) -> Self::State {
            let mut state = state;

            match transition {
                BlockTransition::WatchChanges { watcher_id } => {
                    let initial_version = state.versions[0].clone();

                    // DON'T clone backend - watchers must observe the same backend instance
                    let stream = match state._runtime.block_on(async {
                        tokio::time::timeout(
                            std::time::Duration::from_secs(1),
                            state
                                .backend
                                .watch_changes_since(StreamPosition::Version(initial_version)),
                        )
                        .await
                    }) {
                        Ok(Ok(s)) => s,
                        Ok(Err(e)) => panic!("Failed to create watcher: {:?}", e),
                        Err(_) => panic!(
                            "Timed out creating watcher stream. Likely synchronous replay prefill into a bounded channel in MemoryBackend::watch_changes_since."
                        ),
                    };

                    let mut watcher_stream = WatcherStream {
                        stream: Box::pin(stream),
                    };

                    // Drain initial replay events (limit to 1000 to prevent infinite loops)
                    let mut pending_events = Vec::new();
                    let mut drain_count = 0;
                    const MAX_DRAIN_EVENTS: usize = 1000;

                    loop {
                        if drain_count >= MAX_DRAIN_EVENTS {
                            panic!(
                                "Watcher replay exceeded {} events - possible infinite loop",
                                MAX_DRAIN_EVENTS
                            );
                        }

                        match state._runtime.block_on(async {
                            tokio::time::timeout(
                                std::time::Duration::from_millis(10),
                                watcher_stream.stream.next(),
                            )
                            .await
                        }) {
                            Ok(Some(Ok(event))) => {
                                pending_events.push(event);
                                drain_count += 1;
                            }
                            _ => break,
                        }
                    }

                    let descriptor = WatcherDescriptor {
                        watcher_id: *watcher_id,
                        base_version_idx: 0,
                        last_consumed_idx: state.versions.len() - 1,
                        pending_events,
                    };

                    state.watchers.insert(*watcher_id, descriptor);
                    state.live_streams.insert(*watcher_id, watcher_stream);
                }
                BlockTransition::UnwatchChanges { watcher_id } => {
                    state.watchers.remove(watcher_id);
                    state.live_streams.remove(watcher_id);
                }
                _ => {
                    state._runtime
                        .block_on(apply_transition(&state.backend, transition))
                        .expect("Reference backend transition should succeed (preconditions validated it)");

                    let new_version = state
                        ._runtime
                        .block_on(state.backend.get_current_version())
                        .expect("Failed to get current version");
                    state.versions.push(new_version);

                    // Drain pending events from all active watchers (with safety limit)
                    const MAX_EVENTS_PER_WATCHER: usize = 1000;
                    let watcher_ids: Vec<WatcherId> = state.watchers.keys().copied().collect();

                    for watcher_id in watcher_ids {
                        // Lazy rehydration: rebuild stream if missing (e.g., after Clone)
                        if !state.live_streams.contains_key(&watcher_id)
                            && let Some(descriptor) = state.watchers.get(&watcher_id) {
                                let version = state.versions[descriptor.last_consumed_idx].clone();

                                let stream = match state._runtime.block_on(async {
                                    tokio::time::timeout(
                                        std::time::Duration::from_secs(2),
                                        state
                                            .backend
                                            .watch_changes_since(StreamPosition::Version(version)),
                                    )
                                    .await
                                }) {
                                    Ok(Ok(s)) => s,
                                    Ok(Err(e)) => panic!(
                                        "Failed to rehydrate watcher {}: {:?}",
                                        watcher_id, e
                                    ),
                                    Err(_) => panic!(
                                        "Timed out rehydrating watcher {}. Likely bounded replay prefill deadlock.",
                                        watcher_id
                                    ),
                                };

                                state.live_streams.insert(
                                    watcher_id,
                                    WatcherStream {
                                        stream: Box::pin(stream),
                                    },
                                );
                            }

                        if let Some(stream) = state.live_streams.get_mut(&watcher_id) {
                            let mut event_count = 0;

                            loop {
                                if event_count >= MAX_EVENTS_PER_WATCHER {
                                    panic!(
                                        "Watcher {} received {} events without timeout - possible infinite loop",
                                        watcher_id, MAX_EVENTS_PER_WATCHER
                                    );
                                }

                                match state._runtime.block_on(async {
                                    tokio::time::timeout(
                                        std::time::Duration::from_millis(10),
                                        stream.stream.next(),
                                    )
                                    .await
                                }) {
                                    Ok(Some(Ok(event))) => {
                                        if let Some(descriptor) =
                                            state.watchers.get_mut(&watcher_id)
                                        {
                                            descriptor.pending_events.push(event);
                                            descriptor.last_consumed_idx = state.versions.len() - 1;
                                        }
                                        event_count += 1;
                                    }
                                    _ => break,
                                }
                            }
                        }
                    }
                }
            }

            state
        }
    }

    /// System under test - generic over any backend implementing CoreOperations + Lifecycle
    struct BlockTreeTest<R: CoreOperations + Lifecycle> {
        backend: R,
        /// Initial version for watcher replay
        initial_version: Vec<u8>,
        /// ID mapping: MemoryBackend ID → LoroBackend ID
        id_map: std::collections::HashMap<String, String>,
        /// Watcher notifications: WatcherId → Arc<Mutex<Vec<BlockChange>>>
        watcher_notifications: HashMap<WatcherId, Arc<Mutex<Vec<BlockChange>>>>,
        /// Active watcher stream handles
        watcher_handles: HashMap<WatcherId, tokio::task::JoinHandle<()>>,
        /// Persistent runtime to keep spawned tasks alive
        runtime: Arc<tokio::runtime::Runtime>,
    }

    impl StateMachineTest for BlockTreeTest<LoroBackend> {
        type SystemUnderTest = Self;
        type Reference = ReferenceState;

        fn init_test(
            _ref_state: &<Self::Reference as ReferenceStateMachine>::State,
        ) -> Self::SystemUnderTest {
            let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
            let backend = runtime
                .block_on(LoroBackend::create_new("test-pbt".to_string()))
                .unwrap();

            let initial_version = runtime.block_on(backend.get_current_version()).unwrap();

            BlockTreeTest {
                backend,
                initial_version,
                id_map: HashMap::new(),
                watcher_notifications: HashMap::new(),
                watcher_handles: HashMap::new(),
                runtime,
            }
        }

        fn apply(
            mut state: Self::SystemUnderTest,
            ref_state: &<Self::Reference as ReferenceStateMachine>::State,
            transition: <Self::Reference as ReferenceStateMachine>::Transition,
        ) -> Self::SystemUnderTest {
            let runtime = state.runtime.clone();

            // Translate the transition from MemoryBackend IDs → LoroBackend IDs just-in-time
            let sut_transition = translate_transition(&transition, &state.id_map);

            // Handle watcher commands specially
            match &sut_transition {
                BlockTransition::WatchChanges { watcher_id } => {
                    let notifications = Arc::new(Mutex::new(Vec::new()));
                    let backend_clone = state.backend.clone();
                    let initial_version = state.initial_version.clone();

                    // Create stream and drain initial replay events synchronously,
                    // matching the reference implementation's behavior
                    // Add timeout to prevent deadlock from synchronous replay into bounded channel
                    let mut stream = runtime.block_on(async {
                        tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            backend_clone.watch_changes_since(StreamPosition::Version(initial_version))
                        )
                        .await
                        .expect("Timed out creating SUT watcher stream. Likely bounded channel replay deadlock - watch_changes_since should spawn a task for replay instead of sending synchronously.")
                        .unwrap()
                    });

                    // Drain replay events with timeout (mimics reference implementation)
                    const MAX_DRAIN_EVENTS: usize = 1000;
                    let mut drain_count = 0;
                    loop {
                        if drain_count >= MAX_DRAIN_EVENTS {
                            panic!("Watcher replay exceeded {} events", MAX_DRAIN_EVENTS);
                        }

                        match runtime.block_on(async {
                            tokio::time::timeout(
                                std::time::Duration::from_millis(10),
                                stream.next(),
                            )
                            .await
                        }) {
                            Ok(Some(Ok(event))) => {
                                notifications.lock().unwrap().push(event);
                                drain_count += 1;
                            }
                            _ => break,
                        }
                    }

                    // Spawn task to continue listening for future events
                    let notifications_clone = notifications.clone();
                    let handle = runtime.spawn(async move {
                        while let Some(change_result) = stream.next().await {
                            if let Ok(change) = change_result {
                                notifications_clone.lock().unwrap().push(change);
                            }
                        }
                    });

                    state
                        .watcher_notifications
                        .insert(*watcher_id, notifications);
                    state.watcher_handles.insert(*watcher_id, handle);
                    return state;
                }
                BlockTransition::UnwatchChanges { watcher_id } => {
                    if let Some(handle) = state.watcher_handles.remove(watcher_id) {
                        handle.abort();
                    }
                    state.watcher_notifications.remove(watcher_id);
                    return state;
                }
                _ => {}
            }

            // Apply the translated transition to the SUT
            let created_blocks = runtime
                .block_on(apply_transition(&state.backend, &sut_transition))
                .expect("Transition should succeed on SUT");

            // Update the ID map immediately after create operations
            if !created_blocks.is_empty() {
                let ref_blocks = runtime
                    .block_on(ref_state.backend.get_all_blocks(Traversal::ALL_BUT_ROOT))
                    .unwrap();
                update_id_map_after_create(
                    &mut state.id_map,
                    &transition,
                    &ref_blocks,
                    &created_blocks,
                );
            }

            state
        }

        fn check_invariants(
            state: &Self::SystemUnderTest,
            ref_state: &<Self::Reference as ReferenceStateMachine>::State,
        ) {
            let runtime = tokio::runtime::Runtime::new().unwrap();

            // Compare reference backend (MemoryBackend) with system under test (LoroBackend)
            verify_backends_match(&ref_state.backend, &state.backend, runtime.handle());

            // Give notifications time to propagate (async processing)
            std::thread::sleep(std::time::Duration::from_millis(50));

            // Compare change notification sequences for each watcher
            for (watcher_id, ref_descriptor) in &ref_state.watchers {
                let ref_changes = &ref_descriptor.pending_events;
                let sut_notifications = state
                    .watcher_notifications
                    .get(watcher_id)
                    .unwrap_or_else(|| panic!("Watcher {} should exist in SUT", watcher_id));
                let sut_changes = sut_notifications.lock().unwrap();

                // Both should have received the same number of changes
                assert_eq!(
                    ref_changes.len(),
                    sut_changes.len(),
                    "Reference and SUT should receive same number of change notifications.\nReference: {} changes\nSUT: {} changes",
                    ref_changes.len(),
                    sut_changes.len()
                );

                // Compare each change (accounting for ID mapping)
                for (ref_change, sut_change) in ref_changes.iter().zip(sut_changes.iter()) {
                    match (ref_change, sut_change) {
                        (
                            BlockChange::Created {
                                block: ref_block,
                                origin: ref_origin,
                            },
                            BlockChange::Created {
                                block: sut_block,
                                origin: sut_origin,
                            },
                        ) => {
                            assert_eq!(ref_origin, sut_origin, "Change origins should match");
                            assert_eq!(
                                ref_block.content, sut_block.content,
                                "Block content should match"
                            );
                            // IDs will differ, but should be mapped
                            if let Some(expected_sut_id) = state.id_map.get(&ref_block.id) {
                                assert_eq!(
                                    expected_sut_id, &sut_block.id,
                                    "Block ID mapping should be consistent"
                                );
                            }
                        }
                        (
                            BlockChange::Updated {
                                id: ref_id,
                                content: ref_content,
                                origin: ref_origin,
                            },
                            BlockChange::Updated {
                                id: sut_id,
                                content: sut_content,
                                origin: sut_origin,
                            },
                        ) => {
                            assert_eq!(ref_origin, sut_origin, "Change origins should match");
                            assert_eq!(ref_content, sut_content, "Updated content should match");
                            if let Some(expected_sut_id) = state.id_map.get(ref_id) {
                                assert_eq!(
                                    expected_sut_id, sut_id,
                                    "Updated block ID mapping should be consistent"
                                );
                            }
                        }
                        (
                            BlockChange::Deleted {
                                id: ref_id,
                                origin: ref_origin,
                            },
                            BlockChange::Deleted {
                                id: sut_id,
                                origin: sut_origin,
                            },
                        ) => {
                            assert_eq!(ref_origin, sut_origin, "Change origins should match");
                            if let Some(expected_sut_id) = state.id_map.get(ref_id) {
                                assert_eq!(
                                    expected_sut_id, sut_id,
                                    "Deleted block ID mapping should be consistent"
                                );
                            }
                        }
                        (
                            BlockChange::Moved {
                                id: ref_id,
                                new_parent: ref_parent,
                                after: ref_after,
                                origin: ref_origin,
                            },
                            BlockChange::Moved {
                                id: sut_id,
                                new_parent: sut_parent,
                                after: sut_after,
                                origin: sut_origin,
                            },
                        ) => {
                            assert_eq!(ref_origin, sut_origin, "Change origins should match");
                            if let Some(expected_sut_id) = state.id_map.get(ref_id) {
                                assert_eq!(
                                    expected_sut_id, sut_id,
                                    "Moved block ID mapping should be consistent"
                                );
                            }
                            // Compare parent IDs (accounting for mapping)
                            if let Some(expected) = state.id_map.get(ref_parent) {
                                assert_eq!(
                                    expected, sut_parent,
                                    "Parent ID mapping should be consistent"
                                );
                            }
                            // Compare after IDs (accounting for mapping)
                            match (ref_after, sut_after) {
                                (Some(ref_a), Some(sut_a)) => {
                                    if let Some(expected) = state.id_map.get(ref_a) {
                                        assert_eq!(
                                            expected, sut_a,
                                            "After ID mapping should be consistent"
                                        );
                                    }
                                }
                                (None, None) => {}
                                _ => panic!("After ID presence mismatch in move notification"),
                            }
                        }
                        _ => panic!(
                            "Change notification type mismatch: ref={:?}, sut={:?}",
                            ref_change, sut_change
                        ),
                    }
                }
            }
        }
    }

    proptest_state_machine::prop_state_machine! {
        #![proptest_config(ProptestConfig {
            cases: 5,
            failure_persistence: None,
            timeout: 3000,
            verbose: 2,
            .. ProptestConfig::default()
        })]

        #[test]
        fn test_loro_backend_state_machine(sequential 1..20 => BlockTreeTest<LoroBackend>);
    }
}
