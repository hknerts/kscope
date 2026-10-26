//! One object lined up across a fleet: which members run which images, with
//! how many replicas, and whose spec differs from the rest.
//!
//! "Why does stg-us still run the old build?" and "what did not reach prod?"
//! are questions about drift between clusters, and they are answered by
//! putting the same object from every member side by side. Everything here is
//! pure, over the objects the API servers returned.

use std::collections::HashMap;

use serde_json::Value;

/// What one member's copy of the object says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// Container images, in the order the spec lists them.
    pub images: Vec<String>,
    /// `spec.replicas`, for the kinds that have one.
    pub replicas: Option<i64>,
    /// Hash of what the object asks for: its spec without the replica count
    /// (that has its own column), or the data of a ConfigMap or Secret. Only
    /// compared, never shown, so a Secret's hash does not leave the process.
    pub fingerprint: u64,
    /// What was hashed, kept to name the fields that differ. Field names
    /// only are ever shown, never values.
    asked: Value,
}

/// One member's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Found(Facts),
    /// The member answered: it has no such object.
    Absent,
    /// The member did not answer, or refused.
    Unreachable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub context: String,
    pub state: State,
}

impl Member {
    pub fn new(context: String, answer: Result<Value, String>) -> Self {
        let state = match answer {
            Ok(object) => State::Found(facts(&object)),
            Err(err) if is_not_found(&err) => State::Absent,
            Err(err) => State::Unreachable(err),
        };
        Self { context, state }
    }

    pub fn facts(&self) -> Option<&Facts> {
        match &self.state {
            State::Found(facts) => Some(facts),
            _ => None,
        }
    }
}

fn is_not_found(err: &str) -> bool {
    err.contains("404") || err.contains("NotFound") || err.contains("not found")
}

pub fn facts(object: &Value) -> Facts {
    let asked = asked(object);
    let mut canonical = String::new();
    canonicalize(&asked, &mut canonical);
    Facts {
        images: crate::k8s::resources::container_images(object),
        replicas: object["spec"]["replicas"].as_i64(),
        fingerprint: fnv1a(canonical.as_bytes()),
        asked,
    }
}

/// The part of an object that says what it asks for.
fn asked(object: &Value) -> Value {
    let mut asked = match object["kind"].as_str() {
        Some("ConfigMap" | "Secret") => {
            let mut data = serde_json::Map::new();
            for field in ["data", "binaryData", "stringData"] {
                if let Some(value) = object.get(field) {
                    data.insert(field.to_string(), value.clone());
                }
            }
            Value::Object(data)
        }
        _ => object.get("spec").cloned().unwrap_or(Value::Null),
    };
    if let Some(spec) = asked.as_object_mut() {
        spec.remove("replicas");
    }
    // A `rollout restart` stamps the template; that is not drift.
    if let Some(annotations) = asked
        .pointer_mut("/template/metadata/annotations")
        .and_then(Value::as_object_mut)
    {
        annotations.remove("kubectl.kubernetes.io/restartedAt");
    }
    asked
}

/// The paths at which `a` and `b` differ, e.g.
/// `template.spec.containers[0].args`, at most `limit` of them.
fn differing_paths(a: &Value, b: &Value, path: &str, out: &mut Vec<String>, limit: usize) {
    if a == b || out.len() >= limit {
        return;
    }
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                let (l, r) = (x.get(key), y.get(key));
                differing_paths(
                    l.unwrap_or(&Value::Null),
                    r.unwrap_or(&Value::Null),
                    &child,
                    out,
                    limit,
                );
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (l, r)) in x.iter().zip(y).enumerate() {
                differing_paths(l, r, &format!("{path}[{i}]"), out, limit);
            }
        }
        _ => {
            let name = if path.is_empty() { "(all of it)" } else { path };
            if !out.iter().any(|p| p == name) {
                out.push(name.to_string());
            }
        }
    }
}

/// JSON with object keys sorted, so two equal objects hash the same however
/// the API server ordered their fields.
fn canonicalize(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for key in keys {
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push(':');
                canonicalize(&map[key], out);
                out.push(',');
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for item in items {
                canonicalize(item, out);
                out.push(',');
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
        (hash ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Where the members disagree. Each set holds the members whose value is not
/// the most common one; empty means every member that has the object agrees.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Drift {
    pub images: Vec<String>,
    pub replicas: Vec<String>,
    pub spec: Vec<String>,
    /// Where the odd members' specs differ from the common one, by field.
    pub spec_fields: Vec<String>,
}

impl Drift {
    pub fn is_empty(&self) -> bool {
        self.images.is_empty() && self.replicas.is_empty() && self.spec.is_empty()
    }
}

/// What to say when there is no drift: who has the object, in words.
pub fn agreement(members: &[Member]) -> String {
    let found: Vec<&str> = members
        .iter()
        .filter(|m| m.facts().is_some())
        .map(|m| m.context.as_str())
        .collect();
    match found.len() {
        0 => "no cluster has it".to_string(),
        1 => format!("only {} has it", found[0]),
        n if n == members.len() => format!("the same on all {n} clusters"),
        n => format!("the same on the {n} clusters that have it"),
    }
}

/// At most this many differing fields are named.
const FIELDS_SHOWN: usize = 4;

pub fn drift(members: &[Member]) -> Drift {
    let spec = outliers(members, |f| f.fingerprint);
    let mut spec_fields = Vec::new();
    // Compare each odd member with one that has the common spec.
    let common = members
        .iter()
        .filter(|m| !spec.contains(&m.context))
        .find_map(Member::facts);
    if let Some(common) = common {
        for odd in members.iter().filter(|m| spec.contains(&m.context)) {
            if let Some(facts) = odd.facts() {
                differing_paths(
                    &common.asked,
                    &facts.asked,
                    "",
                    &mut spec_fields,
                    FIELDS_SHOWN,
                );
            }
        }
    }
    Drift {
        images: outliers(members, |f| f.images.clone()),
        replicas: outliers(members, |f| f.replicas),
        spec,
        spec_fields,
    }
}

/// Members whose value differs from the most common one. On a tie the value
/// seen first wins, so the order of the fleet decides, not chance.
fn outliers<T: Eq + std::hash::Hash>(
    members: &[Member],
    value: impl Fn(&Facts) -> T,
) -> Vec<String> {
    let found: Vec<(&str, T)> = members
        .iter()
        .filter_map(|m| m.facts().map(|f| (m.context.as_str(), value(f))))
        .collect();
    let mut counts: HashMap<&T, (usize, usize)> = HashMap::new();
    for (index, (_, v)) in found.iter().enumerate() {
        counts.entry(v).or_insert((0, index)).0 += 1;
    }
    let Some(common) = counts
        .iter()
        .max_by(|a, b| a.1 .0.cmp(&b.1 .0).then(b.1 .1.cmp(&a.1 .1)))
        .map(|(v, _)| *v)
    else {
        return Vec::new();
    };
    found
        .iter()
        .filter(|(_, v)| v != common)
        .map(|(context, _)| context.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn deployment(image: &str, replicas: i64) -> Value {
        json!({
            "kind": "Deployment",
            "spec": {
                "replicas": replicas,
                "template": {
                    "metadata": {"labels": {"app": "checkout"}},
                    "spec": {"containers": [
                        {"name": "app", "image": image},
                        {"name": "proxy", "image": "envoy:1.30"}
                    ]}
                }
            }
        })
    }

    fn member(context: &str, object: Value) -> Member {
        Member::new(context.into(), Ok(object))
    }

    #[test]
    fn finds_images_at_every_depth() {
        assert_eq!(
            facts(&deployment("shop:2", 2)).images,
            vec!["shop:2", "envoy:1.30"]
        );
        let pod = json!({"kind": "Pod", "spec": {"containers": [{"image": "busybox:1.36"}]}});
        assert_eq!(facts(&pod).images, vec!["busybox:1.36"]);
        let cron = json!({"kind": "CronJob", "spec": {"jobTemplate": {"spec": {"template":
            {"spec": {"containers": [{"image": "report:7"}]}}}}}});
        assert_eq!(facts(&cron).images, vec!["report:7"]);
    }

    #[test]
    fn the_odd_one_out_is_flagged() {
        let members = vec![
            member("stg-eu", deployment("shop:2", 2)),
            member("stg-us", deployment("shop:1", 2)),
            member("stg-ap", deployment("shop:2", 2)),
        ];
        let d = drift(&members);
        assert_eq!(d.images, vec!["stg-us"]);
        // A different image is a different spec too, and the field is named.
        assert_eq!(d.spec, vec!["stg-us"]);
        assert_eq!(d.spec_fields, vec!["template.spec.containers[0].image"]);
        assert!(d.replicas.is_empty());
    }

    #[test]
    fn replicas_are_their_own_column_not_spec_drift() {
        let members = vec![
            member("prod-eu", deployment("shop:2", 6)),
            member("prod-us", deployment("shop:2", 3)),
        ];
        let d = drift(&members);
        assert!(d.spec.is_empty() && d.images.is_empty(), "{d:?}");
        // A tie: the first member's value counts as the common one.
        assert_eq!(d.replicas, vec!["prod-us"]);
    }

    #[test]
    fn field_order_and_restarts_are_not_drift() {
        let a = json!({"kind": "Deployment", "spec": {"a": 1, "b": 2,
            "template": {"metadata": {"annotations": {"kubectl.kubernetes.io/restartedAt": "x"}}}}});
        let b = json!({"kind": "Deployment", "spec": {"b": 2, "a": 1,
            "template": {"metadata": {"annotations": {}}}}});
        assert_eq!(facts(&a).fingerprint, facts(&b).fingerprint);
    }

    #[test]
    fn config_data_is_compared() {
        let cm = |v: &str| json!({"kind": "ConfigMap", "data": {"LOG_LEVEL": v}});
        let members = vec![
            member("stg-eu", cm("debug")),
            member("stg-us", cm("info")),
            member("stg-ap", cm("info")),
        ];
        assert_eq!(drift(&members).spec, vec!["stg-eu"]);
    }

    #[test]
    fn agreement_says_who_has_it() {
        let gone = || Member::new("x".into(), Err("not found (404)".into()));
        assert_eq!(agreement(&[gone()]), "no cluster has it");
        assert_eq!(
            agreement(&[member("stg-eu", deployment("a", 1)), gone()]),
            "only stg-eu has it"
        );
        let both = [
            member("a", deployment("a", 1)),
            member("b", deployment("a", 1)),
        ];
        assert_eq!(agreement(&both), "the same on all 2 clusters");
        let mut three = both.to_vec();
        three.push(gone());
        assert_eq!(agreement(&three), "the same on the 2 clusters that have it");
    }

    #[test]
    fn absent_and_unreachable_members_are_kept_apart() {
        let gone = Member::new(
            "a".into(),
            Err("ApiError: deployments \"x\" not found: NotFound (404)".into()),
        );
        assert_eq!(gone.state, State::Absent);
        let down = Member::new("b".into(), Err("no answer in 10s".into()));
        assert!(matches!(down.state, State::Unreachable(_)));
        // Neither takes part in the vote.
        let members = vec![gone, down, member("c", deployment("shop:2", 1))];
        assert!(drift(&members).is_empty());
    }

    fn paths(a: Value, b: Value, limit: usize) -> Vec<String> {
        let mut out = Vec::new();
        differing_paths(&a, &b, "", &mut out, limit);
        out
    }

    #[test]
    fn differing_paths_name_missing_keys_and_resized_lists() {
        assert_eq!(
            paths(
                json!({"a": 1, "b": [1, 2]}),
                json!({"b": [1, 2, 3], "c": 0}),
                10
            ),
            vec!["a", "b", "c"]
        );
        assert_eq!(
            paths(
                json!({"x": [{"y": 1}, {"y": 2}]}),
                json!({"x": [{"y": 1}, {"y": 3}]}),
                10
            ),
            vec!["x[1].y"]
        );
        assert_eq!(paths(json!(1), json!("1"), 10), vec!["(all of it)"]);
        assert!(paths(json!({"a": 1}), json!({"a": 1}), 10).is_empty());
    }

    #[test]
    fn differing_paths_stop_at_the_limit() {
        let a = json!({"a": 1, "b": 1, "c": 1, "d": 1, "e": 1, "f": 1});
        let b = json!({"a": 2, "b": 2, "c": 2, "d": 2, "e": 2, "f": 2});
        assert_eq!(paths(a, b, 4), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn a_secret_drifts_by_field_name_never_by_value() {
        let secret = |v: &str| json!({"kind": "Secret", "data": {"PASSWORD": v}, "type": "Opaque"});
        let members = vec![
            member("a", secret("aHVudGVyMg==")),
            member("b", secret("aHVudGVyMg==")),
            member("c", secret("c3dvcmRmaXNo")),
        ];
        let d = drift(&members);
        assert_eq!(d.spec, vec!["c"]);
        assert_eq!(d.spec_fields, vec!["data.PASSWORD"]);
        assert!(d.spec_fields.iter().all(|f| !f.contains("c3dvcmRmaXNo")));
        assert!(facts(&secret("x")).images.is_empty());
        assert_eq!(facts(&secret("x")).replicas, None);
    }

    #[test]
    fn a_majority_beats_the_first_member() {
        let members = vec![
            member("one", deployment("shop:1", 1)),
            member("two", deployment("shop:2", 1)),
            member("three", deployment("shop:2", 1)),
        ];
        assert_eq!(drift(&members).images, vec!["one"]);
        assert!(drift(&[]).is_empty());
    }

    #[test]
    fn not_found_is_recognised_in_its_usual_spellings() {
        assert!(is_not_found("404 page not found"));
        assert!(is_not_found("NotFound"));
        assert!(!is_not_found("forbidden: User cannot get resource"));
        assert!(!is_not_found("connection refused"));
    }
}
