//! HTTP/JSON importer package files: one `HttpJsonManifest`, two
//! serializations. `.toml` parses directly; `.nix` is evaluated by the
//! in-tree tvix evaluator (`tvix_wasm::eval_to_json`, the same sandboxed
//! entry point `POST /generate` uses) and the resulting JSON goes through the
//! identical validation. Nothing downstream of [`ValidatedPackage`] knows
//! which format a package was authored in.

use std::path::{Path, PathBuf};

use http_json_importer::manifest::HARD_LIMITS;
use http_json_importer::ValidatedPackage;
use serde::{Deserialize, Serialize};

/// Serialization a package file is authored in, selected by file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageFormat {
    Toml,
    Nix,
}

impl PackageFormat {
    /// `.toml` or `.nix`; anything else is a deployment error.
    pub fn from_path(path: &Path) -> Result<Self, String> {
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("toml") => Ok(Self::Toml),
            Some("nix") => Ok(Self::Nix),
            _ => Err(format!(
                "importer package {} must end in .toml or .nix",
                path.display()
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Toml => "toml",
            Self::Nix => "nix",
        }
    }
}

/// A package file's authored source plus where it came from.
#[derive(Debug, Clone)]
pub struct PackageDefinition {
    pub format: PackageFormat,
    pub path: PathBuf,
    pub source: String,
}

/// Parse and validate authored package text. Errors are the validator's
/// (or the Nix evaluator's) message, suitable for surfacing verbatim to the
/// editor.
pub fn parse_http_json_package(
    format: PackageFormat,
    source: &str,
) -> Result<ValidatedPackage, String> {
    if source.len() > HARD_LIMITS.manifest_bytes {
        return Err(format!(
            "package is {} bytes; hard limit is {} bytes",
            source.len(),
            HARD_LIMITS.manifest_bytes
        ));
    }
    match format {
        PackageFormat::Toml => {
            ValidatedPackage::from_toml_bytes(source.as_bytes()).map_err(|error| error.to_string())
        }
        PackageFormat::Nix => {
            let json = tvix_wasm::eval_to_json(source)
                .map_err(|error| format!("Nix evaluation failed: {error}"))?;
            ValidatedPackage::from_json_str(&json).map_err(|error| error.to_string())
        }
    }
}

/// Read a package file's authored text without validating it, so an editor
/// can show (and repair) a package that no longer validates. Enforces the
/// same size ceiling as loading.
pub fn read_package_definition(path: &Path) -> Result<PackageDefinition, String> {
    let format = PackageFormat::from_path(path)?;
    let len = std::fs::metadata(path)
        .map_err(|error| format!("failed to inspect importer package {}: {error}", path.display()))?
        .len() as usize;
    if len > HARD_LIMITS.manifest_bytes {
        return Err(format!(
            "importer package {} is {len} bytes; hard limit is {} bytes",
            path.display(),
            HARD_LIMITS.manifest_bytes
        ));
    }
    let source = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read importer package {}: {error}", path.display()))?;
    Ok(PackageDefinition {
        format,
        path: path.to_path_buf(),
        source,
    })
}

/// Load and validate one package file in either format.
pub fn load_http_json_package(
    path: &Path,
) -> Result<(ValidatedPackage, PackageDefinition), String> {
    let definition = read_package_definition(path)?;
    let package = parse_http_json_package(definition.format, &definition.source)
        .map_err(|error| format!("invalid importer package {}: {error}", path.display()))?;
    Ok((package, definition))
}

/// Replace `path` with `contents` via a sibling temp file + rename, so a
/// concurrent loader sees either the old or the new file, never a torn one.
pub fn write_atomically(path: &Path, contents: &str) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no file name", path.display()))?;
    let temp = dir.join(format!(".{name}.tmp-{}-{}", std::process::id(), unique_suffix()));
    std::fs::write(&temp, contents)
        .map_err(|error| format!("failed to write {}: {error}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("failed to replace {}: {error}", path.display())
    })
}

/// Whether this process can create files in `dir`: probes by creating and
/// removing a hidden file rather than inspecting permission bits, which lie
/// on read-only mounts and ConfigMap projections.
pub fn dir_is_writable(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(format!(".jc-write-probe-{}-{}", std::process::id(), unique_suffix()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_json_importer::manifest::Produces;

    const TOML_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../charts/jump-cannon/packages/hindsight-memory-bank.toml"
    );
    const NIX_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../charts/jump-cannon/packages/hindsight-memory-bank.nix"
    );

    /// (name, node_type, doctype pointer + map) per node collection; edge
    /// collections carry `None`.
    fn collection_shape(package: &ValidatedPackage) -> Vec<(String, Option<(String, Option<(String, Vec<(String, String)>)>)>)> {
        package
            .collections()
            .iter()
            .map(|collection| {
                let nodes = match &collection.produces {
                    Produces::Nodes(rules) => Some((
                        rules.node_type.clone(),
                        rules.doctype.as_ref().map(|rule| {
                            (
                                rule.pointer.clone(),
                                rule.map
                                    .iter()
                                    .map(|(k, v)| (k.clone(), v.clone()))
                                    .collect(),
                            )
                        }),
                    )),
                    Produces::Edges(_) => None,
                };
                (collection.name.clone(), nodes)
            })
            .collect()
    }

    fn schema_keys(package: &ValidatedPackage) -> Vec<String> {
        package
            .manifest()
            .schema
            .fields
            .iter()
            .map(|field| field.key.clone())
            .collect()
    }

    /// The shipped Nix package is the same package as the shipped TOML one.
    #[test]
    fn nix_package_evaluates_to_the_toml_package() {
        let (toml, toml_def) = load_http_json_package(Path::new(TOML_PATH)).expect("TOML loads");
        let (nix, nix_def) = load_http_json_package(Path::new(NIX_PATH)).expect("Nix loads");
        assert_eq!(toml_def.format, PackageFormat::Toml);
        assert_eq!(nix_def.format, PackageFormat::Nix);
        assert!(nix_def.source.contains("doctype"));

        assert_eq!(nix.manifest().metadata.id, toml.manifest().metadata.id);
        assert_eq!(collection_shape(&nix), collection_shape(&toml));
        assert_eq!(schema_keys(&nix), schema_keys(&toml));
        let memories = &collection_shape(&nix)[0];
        assert_eq!(memories.0, "memories");
        let (node_type, doctype) = memories.1.as_ref().expect("memories produces nodes");
        assert_eq!(node_type, "memory");
        let (pointer, map) = doctype.as_ref().expect("memories declares a doctype rule");
        assert_eq!(pointer, "/fact_type");
        assert_eq!(
            map,
            &vec![
                ("experience".to_string(), "Experience".to_string()),
                ("observation".to_string(), "Observation".to_string()),
                ("world".to_string(), "World Fact".to_string()),
            ]
        );
    }

    #[test]
    fn nix_evaluation_errors_are_validation_errors() {
        let error = parse_http_json_package(PackageFormat::Nix, "{ format_version = 1; }")
            .expect_err("incomplete package is rejected");
        assert!(error.contains("missing field"), "{error}");
        let error = parse_http_json_package(PackageFormat::Nix, "throw \"nope\"")
            .expect_err("eval failure is rejected");
        assert!(error.contains("Nix evaluation failed"), "{error}");
    }

    #[test]
    fn unknown_extensions_are_rejected() {
        assert!(PackageFormat::from_path(Path::new("pkg.yaml")).is_err());
        assert_eq!(
            PackageFormat::from_path(Path::new("pkg.nix")).unwrap(),
            PackageFormat::Nix
        );
    }

    #[test]
    fn atomic_write_replaces_and_probe_detects_writability() {
        let dir = std::env::temp_dir().join(format!("jc-pkg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(dir_is_writable(&dir));
        let path = dir.join("a.toml");
        write_atomically(&path, "one").unwrap();
        write_atomically(&path, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        assert!(!dir_is_writable(&dir.join("missing")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
