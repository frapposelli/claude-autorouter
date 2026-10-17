pub fn merge_intervals(intervals: &[[i64; 2]]) -> Vec<[i64; 2]> {
    let mut merged: Vec<[i64; 2]> = Vec::new();
    for &[start, end] in intervals {
        if let Some(previous) = merged.last_mut() {
            if start <= previous[1] {
                previous[1] = end;
                continue;
            }
        }
        merged.push([start, end]);
    }
    merged
}
