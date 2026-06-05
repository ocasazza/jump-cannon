# seed.nix — the initial-position SEED interface + built-in implementations.
#
# A "seed" decides where the N graph nodes START before the force sim takes
# over. It is the layout analogue of graph.nix: a tiny abstract interface
# plus a handful of reference implementations, authored in Nix so a user can
# add their own strategy without touching Rust.
#
# ── The interface ─────────────────────────────────────────────────────────
#
#   seed : { n, ... } -> [ { x; y; z; } ]
#
# A seed is a function from an attrset (which always contains `n`, the node
# count; extra keys are allowed and ignored by the host) to a LIST of exactly
# `n` position attrsets, each `{ x = <number>; y = <number>; z = <number>; }`.
# The host (`tvix_wasm::eval_seed`) validates `length == n`; a shorter or
# longer list is an error (except the empty list, which means "no seed").
# Numbers may be ints or floats — the host coerces to f32.
#
# To register a NEW seed, write a function of this shape and hand its APPLIED
# result to `eval_seed`. The expression's VALUE must be the position list:
#
#   let s = import /jc/src/seed.nix {}; in s.sphere { n = 200; }
#
# or a fully custom one (a flat line along x):
#
#   let n = 64; in builtins.genList (i: { x = i * 10.0; y = 0.0; z = 0.0; }) n
#
# ── Built-in implementations ──────────────────────────────────────────────
#
#   none   : []                         — apply NO seed (leave positions as-is).
#   sphere : Fibonacci sphere shell     — matches Rust spawn_on_unit_sphere.
#   random : deterministic pseudo-random ball (LCG; no impure builtins).
#   grid   : axis-aligned cubic lattice.

{ }:

let
  pi = 3.14159265358979323846;
  sqrt5 = 2.2360679774997896;
  twoPi = 2.0 * pi;

  # ── Pure float helpers ──────────────────────────────────────────────────
  # tvix (default-features = false) ships no math builtins, so sqrt / trig are
  # implemented here. Seeds only need a non-degenerate isotropic spread, so
  # modest accuracy is fine. `builtins.div` on floats truncates toward zero,
  # which is how we get an integer quotient / floor of a positive float.

  # Newton-Raphson sqrt (6 iterations from a crude seed).
  sqrtF = x:
    if x <= 0.0 then 0.0 else
    let
      step = g: 0.5 * (g + x / g);
      g0 = if x > 1.0 then x else 1.0;
      g1 = step g0; g2 = step g1; g3 = step g2;
      g4 = step g3; g5 = step g4; g6 = step g5;
    in g6;

  # Range-reduce an angle to [-pi, pi]. tvix's `builtins.div` does FLOAT
  # division for float args (no truncation), so use `builtins.floor` (which
  # returns an integer) to count whole turns and subtract them off.
  reduce = a:
    let
      turns = builtins.floor (a / twoPi);
      r = a - turns * twoPi;
    in if r > pi then r - twoPi else r;

  # 6-term Taylor sine on the reduced angle.
  sinF = a0:
    let
      a = reduce a0;
      a2 = a * a;
      a3 = a * a2; a5 = a3 * a2; a7 = a5 * a2;
      a9 = a7 * a2; a11 = a9 * a2;
    in a
       - a3 / 6.0
       + a5 / 120.0
       - a7 / 5040.0
       + a9 / 362880.0
       - a11 / 39916800.0;

  cosF = a: sinF (a + pi / 2.0);

  # ── Seeds ───────────────────────────────────────────────────────────────

  # none : leave positions untouched. Returns []; the host treats an empty
  # list as "no seed" (skip the reseed entirely).
  none = { ... }: [ ];

  # sphere : Fibonacci lattice on a sphere shell of `radius` (default 800),
  # matching the Rust `spawn_on_unit_sphere`. Deterministic.
  sphere = { n, radius ? 800.0, ... }:
    let
      phiGolden = pi * (3.0 - sqrt5);
    in
    builtins.genList (i:
      let
        iF = i * 1.0;
        cosPhi = 1.0 - 2.0 * (iF + 0.5) / (n * 1.0);
        s = 1.0 - cosPhi * cosPhi;
        sinPhi = sqrtF (if s < 0.0 then 0.0 else s);
        theta = phiGolden * iF;
      in {
        x = sinPhi * (cosF theta) * radius;
        y = sinPhi * (sinF theta) * radius;
        z = cosPhi * radius;
      }) n;

  # random : deterministic pseudo-random points in a cube of half-extent
  # `radius` (default 800). Per-index LCG so output is stable for a given
  # (n, seed) — no impure builtins.
  random = { n, radius ? 800.0, seed ? 1, ... }:
    let
      # 16-bit LCG (params from Numerical Recipes' ranqd1, masked to 16 bits)
      # so all intermediate products stay well within tvix's i64 range — the
      # full 2654435761-style hashing constants overflow.
      m = 65536;
      modM = v: let q = builtins.div v m; r0 = v - m * q; in
        if r0 < 0 then r0 + m else r0;
      lcg = s: modM (s * 25173 + 13849);
      # Chain a few rounds so consecutive indices decorrelate.
      hash = i: salt:
        let a = lcg (seed * 977 + i * 131 + salt * 31);
            b = lcg (a + i * 17 + salt * 7);
            c = lcg (b + i);
        in (c * 1.0) / (m * 1.0);
    in
    builtins.genList (i: {
      x = (hash i 1 - 0.5) * 2.0 * radius;
      y = (hash i 2 - 0.5) * 2.0 * radius;
      z = (hash i 3 - 0.5) * 2.0 * radius;
    }) n;

  # grid : axis-aligned cubic lattice with `spacing` (default 60) between
  # neighbours, centred on the origin. side = ceil(cbrt n). The smallest
  # integer `side` with side^3 >= n: start from the float cube root and bump
  # up by one if rounding left it short.
  cbrtCeil = n:
    let
      # Newton-Raphson cube root from a crude seed.
      step = g: g - (g * g * g - n * 1.0) / (3.0 * g * g);
      g0 = if n > 1 then n * 1.0 else 1.0;
      g1 = step g0; g2 = step g1; g3 = step g2;
      g4 = step g3; g5 = step g4; g6 = step g5; g7 = step g6;
      c = builtins.ceil g7;
      # guard against tiny undershoot.
    in if c * c * c < n then c + 1 else (if c < 1 then 1 else c);
  grid = { n, spacing ? 60.0, ... }:
    let
      nn = if n < 1 then 1 else n;
      side = cbrtCeil nn;
      half = (side - 1) * spacing / 2.0;
    in
    builtins.genList (i:
      let
        ix = i - side * (builtins.div i side);
        iy = (builtins.div i side) - side * (builtins.div i (side * side));
        iz = builtins.div i (side * side);
      in {
        x = ix * spacing - half;
        y = iy * spacing - half;
        z = iz * spacing - half;
      }) nn;

in {
  inherit none sphere random grid sinF cosF sqrtF;
}
