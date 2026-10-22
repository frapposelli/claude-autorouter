use autorouter_live_fixture::merge_intervals;
#[test]
fn sorts_merges_nested_and_touching_intervals_and_preserves_input() {
    let input = [[8, 10], [1, 5], [2, 3], [5, 7], [12, 15], [9, 11]];
    let original = input;
    assert_eq!(merge_intervals(&input), [[1, 7], [8, 11], [12, 15]]);
    assert_eq!(input, original);
}
#[test]
fn empty_negative_nested_and_duplicate_intervals() {
    assert!(merge_intervals(&[]).is_empty());
    assert_eq!(merge_intervals(&[[1, 9], [2, 3], [4, 5], [1, 9]]), [[1, 9]]);
    assert_eq!(merge_intervals(&[[-2, 1], [-5, -3], [-3, -2]]), [[-5, 1]]);
}
