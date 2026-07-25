//! Rendering an arbitrary API object for the describe tab.
//!
//! `kubectl describe` has a hand-written formatter per kind, which is not a
//! thing kscope can carry for every CRD in a cluster. Instead the object is
//! rendered generically as YAML, with the noise `kubectl` also hides stripped
//! out — managed fields, the last-applied-configuration annotation and the
//! resource version churn that makes two identical objects look different.
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

/// Strip the bookkeeping, then render as YAML.
pub fn render(object: &Value) -> String {
    let mut cleaned = object.clone();
    redact(&mut cleaned);
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

    #[test]
    fn renders_nested_maps() {
        let got =
            render(&json!({"kind": "Pod", "spec": {"nodeName": "n1", "restartPolicy": "Always"}}));
        assert_eq!(
            got,
            "kind: Pod\nspec:\n  nodeName: n1\n  restartPolicy: Always\n"
        );
    }

    #[test]
    fn renders_a_list_of_maps_with_the_first_key_on_the_dash() {
        let got = render(&json!({"containers": [{"name": "app", "image": "busybox"}]}));
        assert_eq!(got, "containers:\n  - image: busybox\n    name: app\n");
    }

    #[test]
    fn renders_scalar_lists() {
        assert_eq!(
            render(&json!({"args": ["sh", "-c", "sleep 1"]})),
            "args:\n  - sh\n  - -c\n  - sleep 1\n"
        );
    }

    #[test]
    fn strips_managed_fields_and_the_last_applied_annotation() {
        let got = render(&json!({
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
        let got = render(&json!({
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
        let got = render(&json!({"a": "8080", "b": "true", "c": "plain", "d": ""}));
        assert_eq!(got, "a: \"8080\"\nb: \"true\"\nc: plain\nd: \"\"\n");
    }

    #[test]
    fn renders_multiline_strings_as_block_scalars() {
        let got = render(&json!({"script": "line one\nline two"}));
        assert_eq!(got, "script: |-\n    line one\n    line two\n");
    }

    #[test]
    fn renders_empty_collections_and_nulls() {
        assert_eq!(
            render(&json!({"a": {}, "b": [], "c": null, "d": 3, "e": true})),
            "a: {}\nb: []\nc: null\nd: 3\ne: true\n"
        );
    }

    #[test]
    fn survives_a_non_object_root() {
        assert_eq!(render(&json!("bare")), "bare\n");
    }
}
