use super::flutter_pbt_backend::FlutterPbtBackend;
use super::flutter_pbt_state_machine::FlutterBlockTreeTest;
/// Manual proptest runner for Flutter PBT
///
/// Since the `prop_state_machine!` macro can't be called from library code,
/// this module provides a manual runner that mimics proptest's behavior.
use rusty_knowledge::api::pbt_infrastructure::*;
use rusty_knowledge::api::repository::CoreOperations;
use std::collections::HashMap;

/// Run a single proptest case with N random transitions
///
/// This manually implements what `prop_state_machine!` does:
/// 1. Generate random transitions using `ReferenceStateMachine::transitions()`
/// 2. Check preconditions
/// 3. Apply to both reference and SUT
/// 4. Check invariants after each step
pub async fn run_single_proptest_case(
    case_num: u32,
    num_steps: usize,
    seed: u64,
    flutter_backend: FlutterPbtBackend,
) -> Result<String, String> {
    eprintln!(
        "\n[PBT Case {}] Starting with {} steps (seed: {})",
        case_num, num_steps, seed
    );

    // 0. Clean up any existing blocks from previous test cases
    let existing_blocks = flutter_backend
        .get_all_blocks(rusty_knowledge::api::Traversal::ALL_BUT_ROOT)
        .await
        .unwrap_or_default();
    let num_existing = existing_blocks.len();
    for block in existing_blocks {
        let _ = flutter_backend.delete_block(&block.id).await;
    }
    eprintln!(
        "[PBT Case {}] Cleaned up {} existing blocks",
        case_num, num_existing
    );

    // 1. Initialize reference state (MemoryBackend)
    let mut ref_state = ReferenceState::default();

    // 2. Initialize SUT state (Flutter UI)
    let mut sut_state = FlutterBlockTreeTest {
        backend: flutter_backend,
        id_map: HashMap::new(),
    };

    // 3. Create proptest test runner with deterministic seed
    use rusty_knowledge::api::pbt_infrastructure::prop::test_runner::{
        Config, RngAlgorithm, TestRng, TestRunner,
    };
    let config = Config {
        cases: 1,
        failure_persistence: None,
        ..Default::default()
    };
    let mut runner =
        TestRunner::new_with_rng(config, TestRng::deterministic_rng(RngAlgorithm::ChaCha));

    let mut actual_steps = 0;
    let mut skipped_transitions = 0;

    // 4. Generate and apply N transitions
    for step in 0..num_steps {
        // Generate a random transition based on current reference state
        let transition_strategy =
            <ReferenceState as ReferenceStateMachine>::transitions(&ref_state);

        let transition = match transition_strategy.new_tree(&mut runner) {
            Ok(tree) => tree.current(),
            Err(e) => {
                return Err(format!(
                    "Case {}: Failed to generate transition at step {}: {}",
                    case_num, step, e
                ));
            }
        };

        // Check preconditions
        if !<ReferenceState as ReferenceStateMachine>::preconditions(&ref_state, &transition) {
            eprintln!(
                "[PBT Case {}] Step {}: Skipped transition (precondition failed): {:?}",
                case_num, step, transition
            );
            skipped_transitions += 1;
            continue;
        }

        eprintln!("[PBT Case {}] Step {}: {:?}", case_num, step, transition);

        // Apply to reference backend
        ref_state = <ReferenceState as ReferenceStateMachine>::apply(ref_state, &transition);

        // Apply to SUT (Flutter)
        sut_state =
            <FlutterBlockTreeTest as StateMachineTest>::apply(sut_state, &ref_state, transition);

        // Check invariants (compare states)
        <FlutterBlockTreeTest as StateMachineTest>::check_invariants(&sut_state, &ref_state);

        actual_steps += 1;
    }

    let summary = format!(
        "Case {} passed: {} steps executed, {} skipped",
        case_num, actual_steps, skipped_transitions
    );
    eprintln!("[PBT Case {}] ✅ {}", case_num, summary);
    Ok(summary)
}

/// Run multiple proptest cases
///
/// Each case gets a different random seed for diversity.
pub async fn run_proptest_cases(
    num_cases: u32,
    steps_per_case: usize,
    backend_factory: impl Fn(u32) -> FlutterPbtBackend,
) -> Result<String, String> {
    let mut results = Vec::new();
    let mut failed_case = None;

    for case_num in 0..num_cases {
        // Create fresh backend for this case
        let backend = backend_factory(case_num);

        // Use case number as seed for reproducibility
        let seed = case_num as u64;

        match run_single_proptest_case(case_num, steps_per_case, seed, backend).await {
            Ok(summary) => {
                results.push(summary);
            }
            Err(e) => {
                failed_case = Some((case_num, e));
                break;
            }
        }
    }

    if let Some((case_num, error)) = failed_case {
        return Err(format!("❌ Case {} FAILED: {}", case_num, error));
    }

    Ok(format!(
        "✅ All {} cases passed! {}",
        num_cases,
        results.join("; ")
    ))
}
