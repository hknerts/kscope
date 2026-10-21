//! Rendering an arbitrary API object for the describe tab.
//!
//! `kubectl describe` has a hand-written formatter per kind, which is not a
//! thing kscope can carry for every CRD in a cluster. Instead the object is
//! rendered generically as YAML, with the noise `kubectl` also hides stripped
//! out — managed fields, the last-applied-configuration annotation and the
//! resource version churn that makes two identical objects look different.
//!
//! Secret values are redacted unless the user opts in: kscope is meant to be
//! safe to hand to anyone, and describe must not be the easiest way to read
//! credentials out of a cluster. Keys and sizes stay visible, because "is the
//! key there, and is it empty?" is what you are usually checking. Revealed
//! values are shown decoded, not as base64.
//!
//! The emitter is written out rather than pulled in: `serde_yaml` is
//! unmaintained, and a read-only viewer should not add an advisory to the
//! dependency audit for the sake of one screen.

use serde_json::Value;

/// Metadata that is bookkeeping rather than information.
const HIDDEN_METADATA: [&str; 4] = ["managedFields", "resourceVersion", "generation", "selfLink"];

/// Annotations that are large, machine-written and never read on screen.
const HIDDEN_ANNOTATIONS: [&str; 2] = [
    "kubectl.kubernetes.io/last-applied-configuration",
    "kubectl.kubernetes.io/restartedAt",
];

/// Strip the bookkeeping, then render as YAML. Secret values are replaced by
/// their size unless `reveal_secrets` is set, and then shown decoded.
pub fn render(object: &Value, reveal_secrets: bool) -> String {
    let mut cleaned = object.clone();
    redact(&mut cleaned);
    if reveal_secrets {
        decode_secret_values(&mut cleaned);
    } else {
        hide_secret_values(&mut cleaned);
    }
    let mut out = String::new();
    emit(&cleaned, 0, &mut out, false);
    out
}

fn redact(value: &mut Value) {
    let Some(root) = value.as_object_mut() else {
        return;
    };
    if let Some(metadata) = root.get_mut("metadata").and_then(Value::as_object_mut) {
        for key in HIDDEN_METADATA {
            metadata.remove(key);
        }
        if let Some(annotations) = metadata
            .get_mut("annotations")
            .and_then(Value::as_object_mut)
        {
            for key in HIDDEN_ANNOTATIONS {
                annotations.remove(key);
            }
            if annotations.is_empty() {
                metadata.remove("annotations");
            }
        }
    }
}

/// A core `v1` Secret: other groups' `Secret` kinds are not this shape.
fn is_secret(object: &Value) -> bool {
    object["kind"] == "Secret" && object["apiVersion"] == "v1"
}

/// Replace every value under `data` (base64) and `stringData` (plain) with a
/// marker that keeps the decoded size.
fn hide_secret_values(object: &mut Value) {
    if !is_secret(object) {
        return;
    }
    for (field, base64) in [("data", true), ("stringData", false)] {
        let Some(map) = object.get_mut(field).and_then(Value::as_object_mut) else {
            continue;
        };
        for value in map.values_mut() {
            let bytes = match value.as_str() {
                Some(s) if base64 => decoded_len(s),
                Some(s) => s.len(),
                None => 0,
            };
            *value = Value::String(format!("<redacted, {bytes} bytes>"));
        }
    }
}

/// Is this object a Secret, whose values `v` can reveal?
pub fn has_secret_values(object: &Value) -> bool {
    is_secret(object)
}

/// Replace every base64 value under `data` with its decoded text. Bytes that
/// are not UTF-8 (keystores, images) are named instead of printed.
fn decode_secret_values(object: &mut Value) {
    use base64::Engine as _;
    if !is_secret(object) {
        return;
    }
    let Some(map) = object.get_mut("data").and_then(Value::as_object_mut) else {
        return;
    };
    for value in map.values_mut() {
        let Some(encoded) = value.as_str() else {
            continue;
        };
        let decoded = match base64::engine::general_purpose::STANDARD.decode(encoded.trim()) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(err) => format!("<binary, {} bytes>", err.as_bytes().len()),
            },
            Err(_) => format!("<not base64: {encoded}>"),
        };
        *value = Value::String(decoded);
    }
}

/// Decoded length of a base64 string, without decoding it.
fn decoded_len(encoded: &str) -> usize {
    let s = encoded.trim();
    let padding = s.bytes().rev().take_while(|&b| b == b'=').count();
    (s.len() / 4 * 3
        + match s.len() % 4 {
            2 => 1,
            3 => 2,
            _ => 0,
        })
    .saturating_sub(padding)
}

/// Emit `value` at `indent`. `inline` means the caller already wrote the key
/// and the opening of this value belongs on that same line.
fn emit(value: &Value, indent: usize, out: &mut String, inline: bool) {
    let pad = "  ".repeat(indent);
    match value {
        Value::Object(map) if map.is_empty() => out.push_str(if inline { " {}\n" } else { "{}\n" }),
        Value::Object(map) => {
            if inline {
                out.push('\n');
            }
            for (key, child) in map {
                out.push_str(&pad);
                out.push_str(key);
                out.push(':');
                match child {
                    Value::Object(_) | Value::Array(_) => emit(child, indent + 1, out, true),
                    _ => {
                        out.push(' ');
                        emit(child, indent + 1, out, true);
                    }
                }
            }
        }
        Value::Array(items) if items.is_empty() => {
            out.push_str(if inline { " []\n" } else { "[]\n" })
        }
        Value::Array(items) => {
            if inline {
                out.push('\n');
            }
            for item in items {
                out.push_str(&pad);
                out.push_str("- ");
                match item {
                    // A nested map's first key goes on the dash line, the rest
                    // line up under it — the usual YAML list-of-maps shape.
                    Value::Object(map) if !map.is_empty() => {
                        let mut nested = String::new();
                        emit(item, indent + 1, &mut nested, false);
                        let mut lines = nested.lines();
                        if let Some(first) = lines.next() {
                            out.push_str(first.trim_start());
                            out.push('\n');
                        }
                        for line in lines {
                            out.push_str(line);
                            out.push('\n');
                        }
                    }
                    _ => emit(item, indent + 1, out, true),
                }
            }
        }
        Value::String(s) => {
            out.push_str(&quote(s));
            out.push('\n');
        }
        Value::Null => out.push_str("null\n"),
        other => {
            out.push_str(&other.to_string());
            out.push('\n');
        }
    }
}

/// Quote only when a bare scalar would be ambiguous or unreadable.
fn quote(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".into();
    }
    // Multi-line strings become YAML block scalars, which is what makes a
    // certificate or a script in a ConfigMap readable instead of one long line.
    if s.contains('\n') {
        let body: Vec<String> = s.lines().map(|l| format!("    {l}")).collect();
        return format!("|-\n{}", body.join("\n"));
    }
    let ambiguous = s.parse::<f64>().is_ok()
        || matches!(
            s.to_ascii_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~"
        )
        || s.starts_with(' ')
        || s.ends_with(' ')
        || s.starts_with([
            '&', '*', '!', '%', '@', '`', '{', '[', '#', '>', '|', '\'', '"',
        ])
        || s.contains(": ")
        || s.contains(" #");
    if ambiguous {
        format!("{s:?}")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Most tests are about the emitter, not redaction.
    fn render_all(value: &Value) -> String {
        render(value, true)
    }

    fn secret() -> Value {
        json!({
            "apiVersion": "v1", "kind": "Secret", "type": "Opaque",
            "metadata": {"name": "db"},
            // "hunter2" and "" in base64.
            "data": {"password": "aHVudGVyMg==", "empty": ""},
            "stringData": {"note": "plain text"}
        })
    }

    #[test]
    fn secret_values_are_redacted_by_default() {
        let got = render(&secret(), false);
        assert!(!got.contains("aHVudGVyMg"), "{got}");
        assert!(!got.contains("plain text"), "{got}");
        assert!(got.contains("password: <redacted, 7 bytes>"), "{got}");
        assert!(got.contains("empty: <redacted, 0 bytes>"), "{got}");
        assert!(got.contains("note: <redacted, 10 bytes>"), "{got}");
        // Everything that is not a value stays readable.
        assert!(got.contains("name: db") && got.contains("type: Opaque"));
    }

    #[test]
    fn secret_values_can_be_revealed_explicitly() {
        let got = render(&secret(), true);
        // Decoded, not base64.
        assert!(got.contains("password: hunter2"), "{got}");
        assert!(!got.contains("aHVudGVyMg"), "{got}");
        assert!(got.contains("note: plain text"), "{got}");
    }

    #[test]
    fn revealed_binary_values_are_named_not_printed() {
        let keystore = json!({
            "apiVersion": "v1", "kind": "Secret",
            // 0xff 0xfe 0x00: not UTF-8.
            "data": {"keystore": "//4A", "bad": "%%%"}
        });
        let got = render(&keystore, true);
        assert!(got.contains("keystore: <binary, 3 bytes>"), "{got}");
        assert!(got.contains("<not base64: %%%>"), "{got}");
    }

    #[test]
    fn only_core_secrets_are_redacted() {
        let configmap = json!({"apiVersion": "v1", "kind": "ConfigMap", "data": {"k": "v"}});
        assert!(render(&configmap, false).contains("k: v"));
        let foreign = json!({"apiVersion": "example.com/v1", "kind": "Secret", "data": {"k": "v"}});
        assert!(render(&foreign, false).contains("k: v"));
    }

    #[test]
    fn computes_base64_decoded_lengths() {
        for (encoded, len) in [
            ("", 0),
            ("YQ==", 1),
            ("YWI=", 2),
            ("YWJj", 3),
            ("YWJjZA==", 4),
        ] {
            assert_eq!(decoded_len(encoded), len, "{encoded}");
        }
    }

    #[test]
    fn renders_nested_maps() {
        let got = render_all(
            &json!({"kind": "Pod", "spec": {"nodeName": "n1", "restartPolicy": "Always"}}),
        );
        assert_eq!(
            got,
            "kind: Pod\nspec:\n  nodeName: n1\n  restartPolicy: Always\n"
        );
    }

    #[test]
    fn renders_a_list_of_maps_with_the_first_key_on_the_dash() {
        let got = render_all(&json!({"containers": [{"name": "app", "image": "busybox"}]}));
        assert_eq!(got, "containers:\n  - image: busybox\n    name: app\n");
    }

    #[test]
    fn renders_scalar_lists() {
        assert_eq!(
            render_all(&json!({"args": ["sh", "-c", "sleep 1"]})),
            "args:\n  - sh\n  - -c\n  - sleep 1\n"
        );
    }

    #[test]
    fn strips_managed_fields_and_the_last_applied_annotation() {
        let got = render_all(&json!({
            "metadata": {
                "name": "api",
                "managedFields": [{"manager": "kubectl"}],
                "resourceVersion": "12345",
                "annotations": {"kubectl.kubernetes.io/last-applied-configuration": "{...}"}
            }
        }));
        assert!(got.contains("name: api"));
        assert!(!got.contains("managedFields"));
        assert!(!got.contains("resourceVersion"));
        // The annotations map was left empty, so it goes too.
        assert!(!got.contains("annotations"));
    }

    #[test]
    fn keeps_annotations_that_still_have_content() {
        let got = render_all(&json!({
            "metadata": {"annotations": {
                "kubectl.kubernetes.io/last-applied-configuration": "{...}",
                "team": "payments"
            }}
        }));
        assert!(got.contains("team: payments"));
        assert!(!got.contains("last-applied"));
    }

    #[test]
    fn quotes_scalars_that_would_otherwise_change_type() {
        // A port of "8080" or a version of "1.30" must not read as a number,
        // and "true" must not read as a boolean.
        let got = render_all(&json!({"a": "8080", "b": "true", "c": "plain", "d": ""}));
        assert_eq!(got, "a: \"8080\"\nb: \"true\"\nc: plain\nd: \"\"\n");
    }

    #[test]
    fn renders_multiline_strings_as_block_scalars() {
        let got = render_all(&json!({"script": "line one\nline two"}));
        assert_eq!(got, "script: |-\n    line one\n    line two\n");
    }

    #[test]
    fn renders_empty_collections_and_nulls() {
        assert_eq!(
            render_all(&json!({"a": {}, "b": [], "c": null, "d": 3, "e": true})),
            "a: {}\nb: []\nc: null\nd: 3\ne: true\n"
        );
    }

    #[test]
    fn survives_a_non_object_root() {
        assert_eq!(render_all(&json!("bare")), "bare\n");
    }
}
