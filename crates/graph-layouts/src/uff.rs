//! UFF (Universal Force Field) bond geometry for the molecular layout.
//!
//! Bond rest lengths come from Rappe et al., *J. Am. Chem. Soc.* 1992,
//! 114, 10024 — UFF equation 3:
//!
//! ```text
//! r_ij = r_i + r_j + r_BO - r_EN
//! r_BO = -λ (r_i + r_j) ln(n)          (λ = 0.1332, n = bond order)
//! r_EN = r_i r_j (√χ_i - √χ_j)² / (χ_i r_i + χ_j r_j)
//! ```
//!
//! with `r` the single-bond radius and `χ` the GMP electronegativity from
//! UFF Table I. Only the elements molecular SDF imports realistically
//! carry are tabulated; unknown elements return `None` and the caller
//! falls back to the global spring length.

/// UFF λ for the bond-order correction (dimensionless).
const LAMBDA: f32 = 0.1332;

/// Per-element UFF parameters: single-bond radius `x` (Å) and
/// electronegativity `χ` (eV, GMP scale).
struct UffAtom {
    x: f32,
    chi: f32,
}

const fn atom(x: f32, chi: f32) -> UffAtom {
    UffAtom { x, chi }
}

/// UFF Table I values for the organic/molecular subset.
fn uff_atom(symbol: &str) -> Option<UffAtom> {
    let table: &[(&str, UffAtom)] = &[
        ("H", atom(0.354, 2.886)),
        ("B", atom(0.838, 4.607)),
        ("C", atom(0.757, 5.343)),
        ("N", atom(0.700, 6.899)),
        ("O", atom(0.658, 8.741)),
        ("F", atom(0.668, 9.240)),
        ("Si", atom(1.117, 4.168)),
        ("P", atom(1.000, 5.464)),
        ("S", atom(1.046, 6.944)),
        ("Cl", atom(1.044, 8.564)),
        ("Br", atom(1.166, 7.946)),
        ("I", atom(1.382, 7.180)),
    ];
    table.iter().find(|(name, _)| *name == symbol).map(|(_, a)| UffAtom { x: a.x, chi: a.chi })
}

/// Normalize an element symbol to UFF spelling: first letter uppercase,
/// rest lowercase (`"CL"` → `"Cl"`). SDF V3000 is already canonical, so
/// this only shields hand-edited inputs.
fn normalize_symbol(raw: &str) -> String {
    let trimmed = raw.trim();
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(first) => {
            let mut symbol = String::with_capacity(trimmed.len());
            symbol.extend(first.to_uppercase());
            symbol.extend(chars.flat_map(char::to_lowercase));
            symbol
        }
        None => String::new(),
    }
}

/// Bond order for an edge kind label (`single`/`double`/`triple`/
/// `aromatic`). Unknown kinds return `None`; callers treat them as
/// single bonds.
pub fn bond_order(kind: &str) -> Option<f32> {
    match kind {
        "single" => Some(1.0),
        "double" => Some(2.0),
        "triple" => Some(3.0),
        "aromatic" => Some(1.5),
        _ => None,
    }
}

/// UFF equilibrium bond length in world units (1 Å = 1 unit) for atoms
/// `a`–`b` at bond order `n`. `None` when either element is untabulated.
pub fn bond_rest_length(a: &str, b: &str, n: f32) -> Option<f32> {
    let a = uff_atom(&normalize_symbol(a))?;
    let b = uff_atom(&normalize_symbol(b))?;
    let r_bo = -LAMBDA * (a.x + b.x) * n.ln();
    let sqrt_diff = a.chi.sqrt() - b.chi.sqrt();
    let r_en = a.x * b.x * sqrt_diff * sqrt_diff / (a.chi * a.x + b.chi * b.x);
    Some(a.x + b.x + r_bo - r_en)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32, tol: f32, label: &str) {
        assert!(
            (actual - expected).abs() <= tol,
            "{label}: expected {expected}, got {actual}"
        );
    }

    #[test]
    fn carbon_carbon_orders_match_uff_equation_3() {
        // Equal elements: r_EN = 0, so r_ij = 2·0.757 − λ·1.514·ln(n).
        assert_close(bond_rest_length("C", "C", 1.0).unwrap(), 1.514, 1e-4, "C-C");
        assert_close(bond_rest_length("C", "C", 2.0).unwrap(), 1.3742, 1e-3, "C=C");
        assert_close(bond_rest_length("C", "C", 3.0).unwrap(), 1.2925, 1e-3, "C≡C");
        assert_close(bond_rest_length("C", "C", 1.5).unwrap(), 1.4322, 1e-3, "C~C");
    }

    #[test]
    fn heteronuclear_length_includes_electronegativity_correction() {
        // C-H: 1.111 - 0 - r_EN ≈ 1.091 (r_EN ≈ 0.0199).
        assert_close(bond_rest_length("C", "H", 1.0).unwrap(), 1.091, 5e-3, "C-H");
        // Shorter than the naive radius sum because of r_EN.
        assert!(bond_rest_length("C", "H", 1.0).unwrap() < 1.111);
    }

    #[test]
    fn rest_length_is_symmetric_in_the_endpoints() {
        assert_eq!(
            bond_rest_length("N", "O", 2.0),
            bond_rest_length("O", "N", 2.0)
        );
    }

    #[test]
    fn unknown_elements_fall_back_to_none() {
        assert_eq!(bond_rest_length("Xx", "C", 1.0), None);
        assert_eq!(bond_rest_length("", "C", 1.0), None);
    }

    #[test]
    fn symbols_are_case_normalized() {
        assert_eq!(
            bond_rest_length("CL", "C", 1.0),
            bond_rest_length("Cl", "C", 1.0)
        );
        assert_eq!(
            bond_rest_length("cl", "C", 1.0),
            bond_rest_length("Cl", "C", 1.0)
        );
    }

    #[test]
    fn edge_kinds_map_to_bond_orders() {
        assert_eq!(bond_order("single"), Some(1.0));
        assert_eq!(bond_order("double"), Some(2.0));
        assert_eq!(bond_order("triple"), Some(3.0));
        assert_eq!(bond_order("aromatic"), Some(1.5));
        assert_eq!(bond_order("owner"), None);
    }
}
