// Source-shared JSON contract comparison. Callers retain their existing names
// and explicit numeric policy; this file exports no runtime library API.
#[allow(unused_macros)]
macro_rules! contract_json_equal {
    ($visibility:vis $name:ident, $a:ident, $b:ident => $number:expr) => {
        $visibility fn $name(left: &serde_json::Value, right: &serde_json::Value) -> bool {
            match (left, right) {
                (serde_json::Value::Number($a), serde_json::Value::Number($b)) => $number,
                (serde_json::Value::Array(a), serde_json::Value::Array(b)) => {
                    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| $name(a, b))
                }
                (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
                    a.len() == b.len()
                        && a.iter().all(|(key, value)| {
                            b.get(key).is_some_and(|other| $name(value, other))
                        })
                }
                _ => left == right,
            }
        }
    };
}

// Retain exact integer identity before attempting an integer/float round trip.
// The open upper bounds prevent saturating casts from hiding a mismatch.
#[allow(unused_macros)]
macro_rules! contract_numeric_equal {
    ($visibility:vis $name:ident) => {
        $visibility fn $name(a: &serde_json::Number, b: &serde_json::Number) -> bool {
            if a == b {
                return true;
            }
            if !a.is_f64() && !b.is_f64() {
                return false;
            }
            let (Some(left), Some(right)) = (a.as_f64(), b.as_f64()) else {
                return false;
            };
            if left != right {
                return false;
            }
            if a.is_f64() && b.is_f64() {
                return true;
            }
            let integer = if a.is_f64() { b } else { a };
            if let Some(value) = integer.as_u64() {
                (0.0..18_446_744_073_709_551_616.0).contains(&left) && left as u64 == value
            } else if let Some(value) = integer.as_i64() {
                (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&left)
                    && left as i64 == value
            } else {
                false
            }
        }
    };
}
