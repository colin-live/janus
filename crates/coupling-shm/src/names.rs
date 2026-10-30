//! Object naming, spec §2.
//!
//! ```text
//! Windows: Local\wcp.<sim_id:016x>.<instance_id>[.s2m|.m2s]
//! Linux:   /wcp.<sim_id:016x>.<instance_id>[.s2m|.m2s]
//! ```
//!
//! `sim_id` is rendered as exactly 16 lower-case hex digits (zero padded) and
//! `instance_id` in decimal. The strings returned here are ready to be passed to
//! [`Segment::create`](crate::Segment::create) / [`Notifier::create`](crate::Notifier::create)
//! and to the matching `open` constructors.
//!
//! The byte-exact format is normative (protocol §2); the golden vectors shared
//! with the Python adapter live in `tests/vectors/names.json` and are asserted
//! by the tests below.

/// All three kernel object names belonging to one model instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentNames {
    /// Memory segment.
    pub segment: String,
    /// Wakeup event "state ready": adapter -> model.
    pub s2m: String,
    /// Wakeup event "effort ready": model -> adapter.
    pub m2s: String,
}

#[cfg(windows)]
const PREFIX: &str = "Local\\";
#[cfg(unix)]
const PREFIX: &str = "/";

/// Build the object names for one model instance (spec §2).
///
/// The protocol pins `sim_id` to 16 lower-case hex digits and `instance_id` to
/// decimal, so Python / C implementations must produce the same bytes.
pub fn names(sim_id: u64, instance_id: u32) -> SegmentNames {
    build(sim_id, instance_id, PREFIX)
}

/// Platform-independent name builder; `prefix` is `Local\` on Windows and `/` on Linux.
fn build(sim_id: u64, instance_id: u32, prefix: &str) -> SegmentNames {
    let segment = format!("{prefix}wcp.{sim_id:016x}.{instance_id}");
    SegmentNames {
        s2m: format!("{segment}.s2m"),
        m2s: format!("{segment}.m2s"),
        segment,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden vectors shared with the Python adapter (B8); see protocol §2.
    const NAME_VECTORS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/names.json"
    ));

    #[derive(Debug, PartialEq, Eq)]
    struct Golden {
        sim_id: u64,
        instance_id: u32,
        windows: Expected,
        linux: Expected,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Expected {
        segment: String,
        s2m: String,
        m2s: String,
    }

    impl Expected {
        fn of(names: &SegmentNames) -> Self {
            Self {
                segment: names.segment.clone(),
                s2m: names.s2m.clone(),
                m2s: names.m2s.clone(),
            }
        }
    }

    /// Minimal reader for the fixed schema of `tests/vectors/names.json`; the
    /// file is small and machine-generated, so no JSON dependency is pulled in.
    fn golden_cases(json: &str) -> Vec<Golden> {
        json.split("\"sim_id\"").skip(1).map(parse_case).collect()
    }

    fn parse_case(chunk: &str) -> Golden {
        let sim_id = u64::from_str_radix(quoted(chunk).trim_start_matches("0x"), 16)
            .expect("sim_id must be a hex number");
        Golden {
            sim_id,
            instance_id: integer(chunk, "\"instance_id\""),
            windows: expected(chunk, "\"windows\""),
            linux: expected(chunk, "\"linux\""),
        }
    }

    fn quoted(s: &str) -> &str {
        let open = s.find('"').expect("opening quote") + 1;
        let close = s[open..].find('"').expect("closing quote") + open;
        &s[open..close]
    }

    fn integer(s: &str, key: &str) -> u32 {
        let rest = s
            .split_once(key)
            .unwrap_or_else(|| panic!("missing {key}"))
            .1;
        let rest = rest
            .split_once(':')
            .unwrap_or_else(|| panic!("missing ':' after {key}"))
            .1;
        let digits: String = rest
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits
            .parse()
            .unwrap_or_else(|e| panic!("invalid integer after {key}: {e}"))
    }

    fn expected(s: &str, key: &str) -> Expected {
        let rest = s
            .split_once(key)
            .unwrap_or_else(|| panic!("missing {key}"))
            .1;
        let open = rest.find('{').expect("object start");
        let close = rest[open..].find('}').expect("object end") + open;
        let body = &rest[open..close];
        Expected {
            segment: string_field(body, "\"segment\""),
            s2m: string_field(body, "\"s2m\""),
            m2s: string_field(body, "\"m2s\""),
        }
    }

    fn string_field(s: &str, key: &str) -> String {
        let rest = s
            .split_once(key)
            .unwrap_or_else(|| panic!("missing {key}"))
            .1;
        // JSON escapes the Windows separator as `Local\\`; unescape to one `\`.
        quoted(rest).replace("\\\\", "\\")
    }

    #[test]
    fn name_shape_is_stable() {
        let n = names(0x0123_4567_89ab_cdef, 42);
        assert!(n.segment.ends_with("wcp.0123456789abcdef.42"));
        assert_eq!(n.s2m, format!("{}.s2m", n.segment));
        assert_eq!(n.m2s, format!("{}.m2s", n.segment));
        #[cfg(windows)]
        assert!(n.segment.starts_with("Local\\"));
        #[cfg(unix)]
        assert!(n.segment.starts_with('/'));
    }

    #[test]
    fn names_are_unique_per_instance() {
        assert_ne!(names(1, 0).segment, names(1, 1).segment);
        assert_ne!(names(1, 0).segment, names(2, 0).segment);
        // zero padded to 16 digits
        assert!(names(1, 0).segment.contains("0000000000000001"));
    }

    #[test]
    fn build_matches_shared_golden_vectors_for_both_prefixes() {
        let cases = golden_cases(NAME_VECTORS);
        assert_eq!(
            cases.len(),
            2,
            "tests/vectors/names.json must contain two cases"
        );

        for case in &cases {
            let windows = build(case.sim_id, case.instance_id, "Local\\");
            assert_eq!(
                Expected::of(&windows),
                case.windows,
                "windows names for sim_id {:#x}",
                case.sim_id
            );

            let linux = build(case.sim_id, case.instance_id, "/");
            assert_eq!(
                Expected::of(&linux),
                case.linux,
                "linux names for sim_id {:#x}",
                case.sim_id
            );
        }
    }

    #[test]
    fn names_uses_the_platform_prefix_from_the_golden_vectors() {
        for case in &golden_cases(NAME_VECTORS) {
            #[cfg(windows)]
            let expected = &case.windows;
            #[cfg(unix)]
            let expected = &case.linux;
            assert_eq!(
                &Expected::of(&names(case.sim_id, case.instance_id)),
                expected,
                "platform names for sim_id {:#x}",
                case.sim_id
            );
        }
    }
}
