//! Importer package files on disk: one `format_version = 3` TOML document per
//! package, validated by `crates/importer` before anything downstream sees it.
//! Reading the authored text and validating it are separate steps so an editor
//! can show and repair a package that no longer validates.

use std::path::{Path, PathBuf};

use importer::{ValidatedPackage, HARD_LIMITS};

/// A package file's authored source plus where it came from.
#[derive(Debug, Clone)]
pub struct PackageDefinition {
    pub path: PathBuf,
    pub source: String,
}

/// Parse and validate authored package text. Errors are the validator's
/// message, suitable for surfacing verbatim to the editor.
pub fn parse_importer_package(source: &str) -> Result<ValidatedPackage, String> {
    if source.len() > HARD_LIMITS.manifest_bytes {
        return Err(format!(
            "package is {} bytes; hard limit is {} bytes",
            source.len(),
            HARD_LIMITS.manifest_bytes
        ));
    }
    ValidatedPackage::from_toml_bytes(source.as_bytes()).map_err(|error| error.to_string())
}

/// Read a package file's authored text without validating it, so an editor
/// can show (and repair) a package that no longer validates. Enforces the
/// same size ceiling as loading.
pub fn read_package_definition(path: &Path) -> Result<PackageDefinition, String> {
    require_toml(path)?;
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
        path: path.to_path_buf(),
        source,
    })
}

/// Load and validate one package file.
pub fn load_importer_package(
    path: &Path,
) -> Result<(ValidatedPackage, PackageDefinition), String> {
    let definition = read_package_definition(path)?;
    let package = parse_importer_package(&definition.source)
        .map_err(|error| format!("invalid importer package {}: {error}", path.display()))?;
    Ok((package, definition))
}

/// Packages are TOML; anything else is a deployment error.
pub fn require_toml(path: &Path) -> Result<(), String> {
    if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
        Ok(())
    } else {
        Err(format!("importer package {} must end in .toml", path.display()))
    }
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
    use importer::json::Produces;

    const TOML_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../charts/jump-cannon/packages/hindsight-memory-bank.toml"
    );

    /// The shipped package loads through the same path `graph-api` boots with,
    /// carrying the `doctype` rule that types Hindsight's memory units.
    #[test]
    fn shipped_package_loads_with_its_doctype_rule() {
        let (package, definition) =
            load_importer_package(Path::new(TOML_PATH)).expect("shipped package loads");
        assert_eq!(package.manifest().metadata.id, "hindsight.memory-bank");
        assert!(definition.source.contains("[parser.collections.nodes.doctype]"));

        let memories = package
            .json_config()
            .expect("shipped package selects the json engine")
            .collections
            .iter()
            .find(|collection| collection.name == "memories")
            .expect("memories collection");
        let Produces::Nodes(rules) = &memories.produces else {
            panic!("memories produces nodes");
        };
        assert_eq!(rules.node_type, "memory");
        let doctype = rules.doctype.as_ref().expect("memories declares a doctype rule");
        assert_eq!(doctype.pointer, "/fact_type");
        assert_eq!(
            doctype.map.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect::<Vec<_>>(),
            vec![
                ("experience", "Experience"),
                ("observation", "Observation"),
                ("world", "World Fact"),
            ]
        );
    }

    #[test]
    fn validation_errors_are_returned_verbatim() {
        let error = parse_importer_package("format_version = 1\n")
            .expect_err("a retired package format is rejected");
        assert!(error.contains("format_version"), "{error}");
        let error = parse_importer_package("format_version = 3\n")
            .expect_err("an incomplete package is rejected");
        assert!(error.contains("parser"), "{error}");
    }

    #[test]
    fn non_toml_extensions_are_rejected() {
        assert!(require_toml(Path::new("pkg.toml")).is_ok());
        for bad in ["pkg.nix", "pkg.yaml", "pkg"] {
            assert!(require_toml(Path::new(bad)).is_err(), "{bad} must be rejected");
        }
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
