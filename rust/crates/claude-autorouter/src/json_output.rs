//! Value-only CLI JSON formatting. Preserve exact native integer variants;
//! format Float variants like JSON.stringify without reparsing report values.
use serde::Serialize;
use serde_json::Value;
use serde_json::ser::{CompactFormatter, Formatter, PrettyFormatter};
use std::io::{self, Write};

struct SourceNumberFormatter<F>(F);
impl<F: Formatter> Formatter for SourceNumberFormatter<F> {
    fn write_f64<W: ?Sized + Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        if !value.is_finite() {
            writer.write_all(b"null")
        } else if value == 0.0 {
            writer.write_all(b"0")
        } else {
            writer.write_all(ryu_js::Buffer::new().format_finite(value).as_bytes())
        }
    }
    // CompactFormatter uses these defaults; these are all nine structural
    // overrides of the pinned PrettyFormatter. Keep just one delegation layer.
    fn begin_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_array(writer)
    }
    fn end_array<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_array(writer)
    }
    fn begin_array_value<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.0.begin_array_value(writer, first)
    }
    fn end_array_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_array_value(writer)
    }
    fn begin_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_object(writer)
    }
    fn end_object<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_object(writer)
    }
    fn begin_object_key<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.0.begin_object_key(writer, first)
    }
    fn begin_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.begin_object_value(writer)
    }
    fn end_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.0.end_object_value(writer)
    }
}

fn render_with<F: Formatter>(report: &Value, formatter: F) -> Result<String, String> {
    let mut bytes = Vec::new();
    let mut serializer =
        serde_json::Serializer::with_formatter(&mut bytes, SourceNumberFormatter(formatter));
    report
        .serialize(&mut serializer)
        .map_err(|_| "Could not format CLI JSON.")?;
    String::from_utf8(bytes).map_err(|_| "Could not format CLI JSON.".into())
}

pub(crate) fn pretty(report: &Value) -> Result<String, String> {
    render_with(report, PrettyFormatter::new())
}

pub(crate) fn compact(report: &Value) -> Result<String, String> {
    render_with(report, CompactFormatter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compact_numbers_preserve_fixed_gates_strings_and_exact_native_integers() {
        // The0.9 fraction is a writer golden, not a changed doctor threshold.
        // Exact u64 preservation is not a Source arbitrary-integer claim.
        let report = json!({"agreement":1.0,"min_agreement":0.9,"under_route_rate":0.0,
            "text":"1.0 -0 1e+21 é😀","integers":[9007199254740993u64,u64::MAX],
            "nested":[-1.25,{"zero":-0.0,"flag":true,"none":null}],
            "small":1e-7,"large":1e21,"empty":[]});
        assert_eq!(
            compact(&report).unwrap(),
            concat!(
                "{\"agreement\":1,\"min_agreement\":0.9,\"under_route_rate\":0,",
                "\"text\":\"1.0 -0 1e+21 é😀\",\"integers\":[9007199254740993,18446744073709551615],",
                "\"nested\":[-1.25,{\"zero\":0,\"flag\":true,\"none\":null}],",
                "\"small\":1e-7,\"large\":1e+21,\"empty\":[]}"
            )
        );
        assert_eq!(report["integers"][0].as_u64(), Some(9007199254740993));
        assert_eq!(report["integers"][1].as_u64(), Some(u64::MAX));
    }
}
