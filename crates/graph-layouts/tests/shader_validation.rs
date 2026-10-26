//! Source-level guards for every WGSL compute kernel under
//! `src/layout/algorithms/shaders`. Two checks, run over every `.wgsl` the
//! directory holds (walked at test time, so a new shader is covered the
//! moment it lands):
//!
//!   1. `all_shaders_parse_and_validate` — naga (the same front end wgpu uses
//!      for native pipelines) must parse and validate the module. Catches
//!      undeclared identifiers, type errors, bad entry points, etc.
//!
//!   2. `all_shaders_parenthesize_mixed_bitwise` — WGSL *requires* parentheses
//!      when a bitwise operator (`^`, `|`) shares an operand with an arithmetic
//!      operator. Chrome's WGSL compiler (Tint) enforces this and rejected
//!      `multilevel.wgsl:212` (`lo * K ^ (...)`) with "mixing '*' and '^'
//!      requires parenthesis", invalidating every multilevel pipeline in the
//!      browser. naga does NOT enforce it: its WGSL grammar is precedence-
//!      climbing (`*` binds tighter than `^`), so it parses the ambiguous form
//!      happily and never surfaces the bug. This lint closes that gap so the
//!      exact class of error fails `cargo test`, not just the browser console.
//!
//! `lint_discriminates_on_the_multilevel_precedence_bug` proves check 2 fails
//! on the pre-fix line and passes the parenthesized fix (and documents that
//! naga accepts both) — the check is shown to discriminate before it is trusted.

use std::path::{Path, PathBuf};

use naga::front::wgsl;
use naga::valid::{Capabilities, ValidationFlags, Validator};

fn shaders_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/layout/algorithms/shaders")
}

fn wgsl_files() -> Vec<PathBuf> {
    let dir = shaders_dir();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read shaders dir {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("wgsl"))
        .collect();
    files.sort();
    files
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("<shader>")
        .to_string()
}

/// Parse + validate `source` exactly as wgpu's native path does.
fn parse_and_validate(label: &str, source: &str) -> Result<(), String> {
    let module = wgsl::parse_str(source)
        .map_err(|e| format!("{label}: WGSL parse error:\n{}", e.emit_to_string(source)))?;
    Validator::new(ValidationFlags::all(), Capabilities::all())
        .validate(&module)
        .map_err(|e| format!("{label}: WGSL validation error:\n{}", e.emit_to_string(source)))?;
    Ok(())
}

#[test]
fn all_shaders_parse_and_validate() {
    let files = wgsl_files();
    // Guard against a silently-empty glob: a vacuous "all pass" is worthless.
    assert!(
        files.len() >= 4,
        "expected at least 4 .wgsl kernels, found {} in {}",
        files.len(),
        shaders_dir().display()
    );
    let names: Vec<String> = files.iter().map(|p| file_label(p)).collect();
    for required in ["multilevel.wgsl", "force.wgsl", "octree.wgsl", "amphiphile.wgsl"] {
        assert!(
            names.iter().any(|n| n == required),
            "shader {required} missing from {}",
            shaders_dir().display()
        );
    }

    let mut failures: Vec<String> = Vec::new();
    for path in &files {
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        if let Err(msg) = parse_and_validate(&file_label(path), &source) {
            failures.push(msg);
        }
    }
    assert!(
        failures.is_empty(),
        "{} shader(s) failed naga parse/validate:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn all_shaders_parenthesize_mixed_bitwise() {
    let mut failures: Vec<String> = Vec::new();
    for path in &wgsl_files() {
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for (line, text) in mixed_bitwise_arith(&source) {
            failures.push(format!("{}:{line}: {text}", file_label(path)));
        }
    }
    assert!(
        failures.is_empty(),
        "{} WGSL expression(s) mix a bitwise operator (^ or |) with arithmetic \
         at the same paren depth — Chrome/Tint rejects this, add parentheses:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

const BAD_LINE: &str =
    "let h = pcg_hash(lo * 0x9E3779B9u ^ (hi * 0x85EBCA6Bu) ^ (round * 0xC2B2AE35u) ^ seed);";
const FIXED_LINE: &str =
    "let h = pcg_hash((lo * 0x9E3779B9u) ^ (hi * 0x85EBCA6Bu) ^ (round * 0xC2B2AE35u) ^ seed);";

fn wrap_for_naga(stmt: &str) -> String {
    format!(
        "fn pcg_hash(v: u32) -> u32 {{ return v; }}\n\
         fn t(lo: u32, hi: u32, round: u32, seed: u32) -> u32 {{\n  {stmt}\n  return h; }}\n"
    )
}

#[test]
fn lint_discriminates_on_the_multilevel_precedence_bug() {
    // The lint flags the exact pre-fix form and accepts the parenthesized fix.
    assert!(
        !mixed_bitwise_arith(BAD_LINE).is_empty(),
        "lint must flag the pre-fix multilevel.wgsl:212 operator mix"
    );
    assert!(
        mixed_bitwise_arith(FIXED_LINE).is_empty(),
        "lint must accept the parenthesized fix"
    );
    // naga, by contrast, accepts BOTH (precedence-climbing, not Tint's spec
    // grammar) — which is precisely why the lint is needed on top of it.
    assert!(
        wgsl::parse_str(&wrap_for_naga(BAD_LINE)).is_ok(),
        "naga was expected to parse the ambiguous form (it uses precedence, not Tint's rule)"
    );
    parse_and_validate("fixed", &wrap_for_naga(FIXED_LINE))
        .expect("the parenthesized fix must parse and validate under naga too");
}

// --- the lint -------------------------------------------------------------------

#[derive(Default, Clone, Copy)]
struct Frame {
    arith: bool,
    bitwise: bool,
}

/// Replace `//` line and `/* */` block comments with spaces, preserving
/// newlines so line numbers stay accurate. Shaders are ASCII outside comments.
fn strip_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                if b[i] == b'\n' {
                    out.push(b'\n');
                }
                i += 1;
            }
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Return `(line, trimmed_text)` for every expression that places a bitwise
/// operator (`^`, or `|` that is not `||`/`|=`) and an arithmetic operator
/// (`+ - * / %` in binary position) at the *same* parenthesis depth without
/// grouping parentheses — the WGSL ambiguity Tint rejects.
fn mixed_bitwise_arith(src: &str) -> Vec<(usize, String)> {
    let stripped = strip_comments(src);
    let lines: Vec<&str> = stripped.lines().collect();
    let b = stripped.as_bytes();
    let mut stack: Vec<Frame> = vec![Frame::default()];
    let mut prev_value = false; // did the previous token yield a value?
    let mut line = 1usize;
    let mut out: Vec<(usize, String)> = Vec::new();

    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let next_is = |i: usize, c: u8| i + 1 < b.len() && b[i + 1] == c;

    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            line += 1;
            i += 1;
            prev_value = false;
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if is_word(c) {
            while i < b.len() && (is_word(b[i]) || b[i] == b'.') {
                i += 1;
            }
            prev_value = true;
            continue;
        }
        match c {
            b'.' => {
                // member access / float point: still a value context
                i += 1;
            }
            b'(' | b'[' => {
                stack.push(Frame::default());
                i += 1;
                prev_value = false;
            }
            b')' | b']' => {
                if stack.len() > 1 {
                    stack.pop();
                }
                i += 1;
                prev_value = true;
            }
            b'{' | b'}' | b';' | b',' => {
                if let Some(f) = stack.last_mut() {
                    *f = Frame::default();
                }
                i += 1;
                prev_value = false;
            }
            b'^' => {
                if next_is(i, b'=') {
                    reset_top(&mut stack);
                    i += 2;
                } else {
                    mark(&mut stack, &mut out, line, &lines, false, true);
                    i += 1;
                }
                prev_value = false;
            }
            b'|' => {
                if next_is(i, b'|') {
                    i += 2; // logical or — not a bitwise mix
                } else if next_is(i, b'=') {
                    reset_top(&mut stack);
                    i += 2;
                } else {
                    mark(&mut stack, &mut out, line, &lines, false, true);
                    i += 1;
                }
                prev_value = false;
            }
            b'*' | b'/' | b'%' => {
                if next_is(i, b'=') {
                    reset_top(&mut stack);
                    i += 2;
                } else {
                    // `*`/`/` in non-value position would be a unary deref —
                    // not an arithmetic binary op. `%` is always binary.
                    if c == b'%' || prev_value {
                        mark(&mut stack, &mut out, line, &lines, true, false);
                    }
                    i += 1;
                }
                prev_value = false;
            }
            b'+' => {
                if next_is(i, b'=') {
                    reset_top(&mut stack);
                    i += 2;
                } else {
                    if prev_value {
                        mark(&mut stack, &mut out, line, &lines, true, false);
                    }
                    i += 1;
                }
                prev_value = false;
            }
            b'-' => {
                if next_is(i, b'=') {
                    reset_top(&mut stack);
                    i += 2;
                } else if next_is(i, b'>') {
                    i += 2; // return-type arrow `->`
                } else {
                    if prev_value {
                        mark(&mut stack, &mut out, line, &lines, true, false);
                    }
                    i += 1;
                }
                prev_value = false;
            }
            b'=' => {
                if next_is(i, b'=') {
                    i += 2; // comparison
                } else {
                    reset_top(&mut stack); // assignment: new expression
                    i += 1;
                }
                prev_value = false;
            }
            b'<' | b'>' => {
                // comparison / shift / generic brackets — never classified, but
                // consume the whole run so a trailing `=` isn't misread.
                while i < b.len() && matches!(b[i], b'<' | b'>' | b'=') {
                    i += 1;
                }
                prev_value = false;
            }
            b'&' => {
                // `&&` logical, `&=` assign, `&` address-of or bitwise-and —
                // address-of is unary and common, so `&` is never classified.
                if next_is(i, b'&') {
                    i += 2;
                } else if next_is(i, b'=') {
                    reset_top(&mut stack);
                    i += 2;
                } else {
                    i += 1;
                }
                prev_value = false;
            }
            b'!' => {
                i += if next_is(i, b'=') { 2 } else { 1 };
                prev_value = false;
            }
            _ => {
                i += 1;
                prev_value = false;
            }
        }
    }
    out
}

fn reset_top(stack: &mut [Frame]) {
    if let Some(f) = stack.last_mut() {
        *f = Frame::default();
    }
}

fn mark(
    stack: &mut [Frame],
    out: &mut Vec<(usize, String)>,
    line: usize,
    lines: &[&str],
    arith: bool,
    bitwise: bool,
) {
    let Some(f) = stack.last_mut() else { return };
    f.arith |= arith;
    f.bitwise |= bitwise;
    if f.arith && f.bitwise && out.last().map(|(l, _)| *l) != Some(line) {
        let text = lines.get(line - 1).map(|s| s.trim().to_string()).unwrap_or_default();
        out.push((line, text));
    }
}
