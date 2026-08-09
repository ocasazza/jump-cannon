//! RayCluster session template: load, validate, and stamp the chart-rendered
//! manifest mounted into the graph-api pod.
//!
//! The Helm chart owns *what* the session cluster looks like; graph-api owns
//! *when* it exists. The template is treated as opaque JSON (`serde_json::Value`)
//! — never typed KubeRay structs — so CRD version skew cannot break us (R12).
//! Validation happens once at boot; a bad template fails the session FEATURE
//! loudly (logged, `enabled:false`) and never panics the server.

use std::path::Path;

use anyhow::{anyhow, bail, Context};
use serde_json::Value;

/// Kueue queue label — must be present in the chart-rendered template so the
/// RayCluster is admitted through the ClusterQueue (and Kueue owns
/// `spec.suspend`, which we never touch).
pub const QUEUE_LABEL: &str = "kueue.x-k8s.io/queue-name";
/// Kueue-enforced hard execution cap. Injected at stamp time when the chart
/// did not set it (default matches the controller-side max session length).
pub const MAX_EXEC_LABEL: &str = "kueue.x-k8s.io/max-exec-time-seconds";

/// A validated RayCluster manifest ready to stamp and POST via the kube
/// dynamic API.
#[derive(Clone, Debug)]
pub struct SessionTemplate {
    manifest: Value,
}

impl SessionTemplate {
    /// Read + parse + validate the mounted template file. Returns an error
    /// (never panics) on any IO/parse/validation failure; the caller disables
    /// the session feature on Err.
    pub fn load(path: &Path, cluster_name: &str, namespace: &str) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read gpu session template {}", path.display()))?;
        // The chart mounts the manifest as YAML (raycluster.yaml); YAML is a
        // superset of JSON, so this accepts either rendering.
        let manifest: Value = serde_yaml::from_str(&raw).with_context(|| {
            format!(
                "parse gpu session template {} as YAML",
                path.display()
            )
        })?;
        Self::validate(&manifest, cluster_name, namespace)
            .with_context(|| format!("invalid gpu session template {}", path.display()))?;
        Ok(Self { manifest })
    }

    /// Boot-time validation per the locked spec: parses as an object,
    /// `kind == "RayCluster"`, apiVersion in the ray.io group, metadata
    /// name/namespace match the configured values, and the Kueue queue label
    /// is present.
    pub fn validate(manifest: &Value, cluster_name: &str, namespace: &str) -> anyhow::Result<()> {
        let kind = manifest
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing kind"))?;
        if kind != "RayCluster" {
            bail!("kind must be RayCluster, got {kind:?}");
        }
        let api_version = manifest
            .get("apiVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing apiVersion"))?;
        if !api_version.starts_with("ray.io/") {
            bail!("apiVersion must be in the ray.io group, got {api_version:?}");
        }
        let name = manifest
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing metadata.name"))?;
        if name != cluster_name {
            bail!("metadata.name {name:?} does not match configured cluster name {cluster_name:?}");
        }
        let ns = manifest
            .pointer("/metadata/namespace")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing metadata.namespace"))?;
        if ns != namespace {
            bail!("metadata.namespace {ns:?} does not match configured namespace {namespace:?}");
        }
        let labels = manifest
            .pointer("/metadata/labels")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("missing metadata.labels"))?;
        if !labels.contains_key(QUEUE_LABEL) {
            bail!("metadata.labels must include {QUEUE_LABEL} (Kueue admission gate)");
        }
        Ok(())
    }

    /// Clone the template and stamp the control-plane-owned fields:
    /// `metadata.name`, `metadata.namespace`, and the max-exec-time label
    /// (injected with `max_exec_seconds` when the chart left it unset; an
    /// explicitly chart-set value is kept). Everything else is verbatim —
    /// the chart owns the cluster shape.
    pub fn render(&self, cluster_name: &str, namespace: &str, max_exec_seconds: u64) -> Value {
        let mut out = self.manifest.clone();
        // Validation guarantees these paths exist as objects/strings.
        if let Some(metadata) = out.get_mut("metadata").and_then(Value::as_object_mut) {
            metadata.insert("name".into(), Value::String(cluster_name.to_string()));
            metadata.insert("namespace".into(), Value::String(namespace.to_string()));
            let labels = metadata
                .entry("labels".to_string())
                .or_insert_with(|| Value::Object(Default::default()));
            if let Some(labels) = labels.as_object_mut() {
                labels
                    .entry(MAX_EXEC_LABEL.to_string())
                    .or_insert_with(|| Value::String(max_exec_seconds.to_string()));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn good_manifest() -> Value {
        json!({
            "apiVersion": "ray.io/v1",
            "kind": "RayCluster",
            "metadata": {
                "name": "jump-cannon-compute",
                "namespace": "gpu-workloads",
                "labels": {
                    "kueue.x-k8s.io/queue-name": "gpu",
                    "app.kubernetes.io/name": "jump-cannon"
                }
            },
            "spec": { "headGroupSpec": {} }
        })
    }

    #[test]
    fn validate_accepts_chart_rendered_manifest() {
        SessionTemplate::validate(&good_manifest(), "jump-cannon-compute", "gpu-workloads")
            .expect("valid manifest");
    }

    #[test]
    fn validate_rejects_wrong_kind() {
        let mut m = good_manifest();
        m["kind"] = json!("RayJob");
        let err = SessionTemplate::validate(&m, "jump-cannon-compute", "gpu-workloads")
            .expect_err("wrong kind must fail");
        assert!(err.to_string().contains("RayCluster"), "{err}");
    }

    #[test]
    fn validate_rejects_non_ray_group() {
        let mut m = good_manifest();
        m["apiVersion"] = json!("apps/v1");
        assert!(
            SessionTemplate::validate(&m, "jump-cannon-compute", "gpu-workloads").is_err()
        );
    }

    #[test]
    fn validate_rejects_name_mismatch() {
        let err = SessionTemplate::validate(&good_manifest(), "other-compute", "gpu-workloads")
            .expect_err("name mismatch must fail");
        assert!(err.to_string().contains("metadata.name"), "{err}");
    }

    #[test]
    fn validate_rejects_namespace_mismatch() {
        assert!(
            SessionTemplate::validate(&good_manifest(), "jump-cannon-compute", "default").is_err()
        );
    }

    #[test]
    fn validate_rejects_missing_queue_label() {
        let mut m = good_manifest();
        m.pointer_mut("/metadata/labels")
            .and_then(Value::as_object_mut)
            .expect("labels")
            .remove(QUEUE_LABEL);
        let err = SessionTemplate::validate(&m, "jump-cannon-compute", "gpu-workloads")
            .expect_err("missing queue label must fail");
        assert!(err.to_string().contains(QUEUE_LABEL), "{err}");
    }

    /// Label values carry `/` (JSON-pointer path separator), so tests index
    /// through objects rather than `Value::pointer`.
    fn label<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
        v.pointer("/metadata/labels")?
            .as_object()?
            .get(key)?
            .as_str()
    }

    #[test]
    fn render_stamps_identity_and_injects_max_exec_label() {
        let template = SessionTemplate {
            manifest: good_manifest(),
        };
        let stamped = template.render("jump-cannon-compute", "gpu-workloads", 14400);
        assert_eq!(
            stamped.pointer("/metadata/name").and_then(Value::as_str),
            Some("jump-cannon-compute")
        );
        assert_eq!(
            stamped
                .pointer("/metadata/namespace")
                .and_then(Value::as_str),
            Some("gpu-workloads")
        );
        assert_eq!(label(&stamped, MAX_EXEC_LABEL), Some("14400"));
        // Untouched fields survive verbatim (chart owns the shape).
        assert_eq!(
            stamped.pointer("/spec/headGroupSpec"),
            Some(&json!({}))
        );
        // The template itself is not mutated by render.
        assert!(label(&template.manifest, MAX_EXEC_LABEL).is_none());
    }

    #[test]
    fn render_keeps_chart_set_max_exec_label() {
        let mut m = good_manifest();
        m.pointer_mut("/metadata/labels")
            .and_then(Value::as_object_mut)
            .expect("labels")
            .insert(MAX_EXEC_LABEL.into(), json!("7200"));
        let template = SessionTemplate { manifest: m };
        let stamped = template.render("jump-cannon-compute", "gpu-workloads", 14400);
        assert_eq!(label(&stamped, MAX_EXEC_LABEL), Some("7200"));
    }

    #[test]
    fn load_rejects_nonexistent_file_and_invalid_manifest() {
        let dir = std::env::temp_dir().join(format!("gpu-session-template-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let missing = dir.join("nope.yaml");
        assert!(SessionTemplate::load(&missing, "a", "b").is_err());
        // Parses as YAML but fails validation (name/namespace mismatch).
        let invalid = dir.join("invalid.yaml");
        std::fs::write(&invalid, "kind: RayCluster\n").expect("write");
        assert!(SessionTemplate::load(&invalid, "a", "b").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_accepts_chart_yaml_rendering() {
        let dir = std::env::temp_dir().join(format!("gpu-session-template-yaml-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("raycluster.yaml");
        std::fs::write(
            &path,
            "apiVersion: ray.io/v1\nkind: RayCluster\nmetadata:\n  name: jump-cannon-compute\n  namespace: gpu-workloads\n  labels:\n    kueue.x-k8s.io/queue-name: gpu\nspec: {}\n",
        )
        .expect("write");
        let template = SessionTemplate::load(&path, "jump-cannon-compute", "gpu-workloads")
            .expect("chart-style YAML template loads");
        let stamped = template.render("jump-cannon-compute", "gpu-workloads", 14400);
        assert_eq!(label(&stamped, MAX_EXEC_LABEL), Some("14400"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
