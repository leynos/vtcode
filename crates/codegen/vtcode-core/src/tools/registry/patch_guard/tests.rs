use super::*;
use proptest::prelude::*;
use rstest::rstest;

/// Build a single-path state fixture for the guard property tests.
fn state(path: &str, hash: &str) -> PatchPathState {
    PatchPathState::new(PathBuf::from(path), true, hash.to_string())
}

/// Build a one-operation fixture targeting the given path.
fn operation(path: &str) -> Vec<CanonicalPatchOperation> {
    vec![CanonicalPatchOperation { source: PathBuf::from(path), destination: None }]
}

/// Build a canonical update operation fixture for the given path.
fn update_operation(path: &str) -> CanonicalPatchOperation {
    CanonicalPatchOperation { source: PathBuf::from(path), destination: None }
}

/// Equivalent spellings of one edit must share a signature.
///
/// The transport-alias case checks LF, CRLF, and `./` spellings of the same
/// path normalise to one signature. The independent-operations case checks a
/// swapped listing of unrelated updates hashes identically.
#[rstest]
#[case::transport_and_path_aliases(
    "*** Begin Patch\n*** Update File: x.txt\n@@\n-x\n+x\n*** End Patch\n",
    "*** Begin Patch\r\n*** Update File: ./x.txt\r\n@@\r\n-x\r\n+x\r\n*** End Patch\r\n",
    vec![update_operation("/workspace/x.txt")],
    vec![update_operation("/workspace/x.txt")],
    vec![state("/workspace/x.txt", "same")]
)]
#[case::independent_operations_in_swapped_order(
    concat!(
        "*** Begin Patch\n",
        "*** Update File: a.txt\n",
        "@@\n",
        "-alpha\n",
        "+alpha\n",
        "*** Update File: b.txt\n",
        "@@\n",
        "-beta\n",
        "+beta\n",
        "*** End Patch\n",
    ),
    concat!(
        "*** Begin Patch\n",
        "*** Update File: b.txt\n",
        "@@\n",
        "-beta\n",
        "+beta\n",
        "*** Update File: a.txt\n",
        "@@\n",
        "-alpha\n",
        "+alpha\n",
        "*** End Patch\n",
    ),
    vec![update_operation("/workspace/a.txt"), update_operation("/workspace/b.txt")],
    vec![update_operation("/workspace/b.txt"), update_operation("/workspace/a.txt")],
    vec![state("/workspace/a.txt", "same-a"), state("/workspace/b.txt", "same-b")]
)]
fn equivalent_spellings_share_one_signature(
    #[case] left_patch: &str,
    #[case] right_patch: &str,
    #[case] left_operations: Vec<CanonicalPatchOperation>,
    #[case] right_operations: Vec<CanonicalPatchOperation>,
    #[case] states: Vec<PatchPathState>,
) {
    let left = Patch::parse(left_patch).expect("valid left patch");
    let right = Patch::parse(right_patch).expect("valid right patch");

    assert_eq!(
        patch_signature(&left, &left_operations, &states, &states),
        patch_signature(&right, &right_operations, &states, &states)
    );
}

/// Check updates to a common path keep their supplied relative order.
#[test]
fn dependent_operations_keep_their_supplied_order() {
    let forward = Patch::parse(concat!(
        "*** Begin Patch\n",
        "*** Update File: a.txt\n",
        "@@\n",
        "-one\n",
        "+ONE\n",
        "*** Update File: a.txt\n",
        "@@\n",
        "-two\n",
        "+TWO\n",
        "*** End Patch\n",
    ))
    .expect("valid forward patch");
    let reverse = Patch::parse(concat!(
        "*** Begin Patch\n",
        "*** Update File: a.txt\n",
        "@@\n",
        "-two\n",
        "+TWO\n",
        "*** Update File: a.txt\n",
        "@@\n",
        "-one\n",
        "+ONE\n",
        "*** End Patch\n",
    ))
    .expect("valid reverse patch");
    let operations = vec![
        update_operation("/workspace/a.txt"),
        update_operation("/workspace/a.txt"),
    ];
    let states = vec![state("/workspace/a.txt", "same")];

    assert_ne!(
        patch_signature(&forward, &operations, &states, &states),
        patch_signature(&reverse, &operations, &states, &states),
        "operations sharing a path must keep their supplied order"
    );
}

proptest! {
    /// Check the signature ignores the order paths are listed in.
    #[test]
    fn patch_guard_prop_signature_is_path_order_invariant(
        left_path in "[a-z]{1,8}\\.txt",
        right_path in "[a-z]{1,8}\\.txt",
        left_hash in "[0-9a-f]{64}",
        right_hash in "[0-9a-f]{64}",
    ) {
        prop_assume!(left_path != right_path);
        let patch = Patch::parse("*** Begin Patch\n*** Add File: x.txt\n+x\n*** End Patch\n")
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let forward = vec![state(&left_path, &left_hash), state(&right_path, &right_hash)];
        let reverse = vec![state(&right_path, &right_hash), state(&left_path, &left_hash)];
        let operations = operation("x.txt");
        prop_assert_eq!(
            patch_signature(&patch, &operations, &forward, &forward),
            patch_signature(&patch, &operations, &reverse, &reverse)
        );
    }

    /// Check swapping two independent operations cannot change the signature.
    #[test]
    fn patch_guard_prop_signature_is_independent_operation_order_invariant(
        left_path in "[a-z]{1,8}\\.txt",
        right_path in "[a-z]{1,8}\\.txt",
        left_body in "[a-z]{1,12}",
        right_body in "[a-z]{1,12}",
    ) {
        prop_assume!(left_path != right_path);
        let forward = Patch::parse(&format!(
            "*** Begin Patch\n*** Add File: {left_path}\n+{left_body}\n\
             *** Add File: {right_path}\n+{right_body}\n*** End Patch\n"
        ))
        .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let reverse = Patch::parse(&format!(
            "*** Begin Patch\n*** Add File: {right_path}\n+{right_body}\n\
             *** Add File: {left_path}\n+{left_body}\n*** End Patch\n"
        ))
        .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let forward_operations = vec![
            update_operation(&format!("/workspace/{left_path}")),
            update_operation(&format!("/workspace/{right_path}")),
        ];
        let reverse_operations = vec![
            update_operation(&format!("/workspace/{right_path}")),
            update_operation(&format!("/workspace/{left_path}")),
        ];
        let states = vec![
            state(&format!("/workspace/{left_path}"), "same"),
            state(&format!("/workspace/{right_path}"), "same"),
        ];
        prop_assert_eq!(
            patch_signature(&forward, &forward_operations, &states, &states),
            patch_signature(&reverse, &reverse_operations, &states, &states)
        );
    }

    /// Check a changed patch or file state resets the no-op ladder.
    #[test]
    fn patch_guard_prop_state_or_patch_changes_reset_the_ladder(
        suffix in "[a-z]{1,12}",
    ) {
        prop_assume!(suffix != "x");
        let first_patch = Patch::parse("*** Begin Patch\n*** Add File: x.txt\n+x\n*** End Patch\n")
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let second_patch = Patch::parse(&format!("*** Begin Patch\n*** Add File: x.txt\n+{suffix}\n*** End Patch\n"))
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let first_state = vec![state("x.txt", "first")];
        let second_state = vec![state("x.txt", "second")];
        let operations = operation("x.txt");
        let mut guard = NoOpPatchGuard::default();
        let _ = guard.observe(&first_patch, &operations, &first_state, &first_state);
        let _ = guard.observe(&first_patch, &operations, &first_state, &first_state);

        prop_assert!(matches!(
            guard.observe(&second_patch, &operations, &first_state, &first_state),
            NoOpPatchDecision::Success { occurrence: 1, .. }
        ), "changed patch should reset the ladder");
        prop_assert!(matches!(
            guard.observe(&second_patch, &operations, &second_state, &second_state),
            NoOpPatchDecision::Success { occurrence: 1, .. }
        ), "changed file state should reset the ladder");
    }
}
