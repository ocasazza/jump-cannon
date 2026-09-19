# WaterMap Integration in Glide WS: Mathematical and Algorithmic Foundations

## Overview

WaterMap is the computational engine that makes Glide WS possible. It computes the thermodynamic profile of every water molecule in a protein binding site using a combination of **Grand Canonical Monte Carlo (GCMC) molecular dynamics** and **Inhomogeneous Solvation Theory (IST)**. The result is a 3D map of "hydration sites" — positions where water molecules are thermodynamically stable — each annotated with enthalpy, entropy, and free energy.

This document explains how WaterMap works, how its output is integrated into the Glide WS scoring function, and why this integration represents a genuine advance over both implicit solvent and simpler explicit water methods.

## Part 1: The WaterMap Computational Pipeline

### 1.1 System Preparation

WaterMap operates on a solvated protein binding site. The preparation steps:

1. **Protein structure**: An X-ray, cryo-EM, or high-quality homology model of the target. Crystallographic waters within 5 Å of the binding site are retained; all others are removed.

2. **Solvation**: The binding site is immersed in a sphere of TIP4P (or SPC) water molecules with radius 10–15 Å from the geometric center of the binding site. The sphere is large enough to include all waters that may interact with a bound ligand plus a buffer of bulk-like solvent.

3. **Restraints**: Protein heavy atoms beyond 5 Å from the sphere center are harmonically restrained (force constant ~5 kcal/mol/Å²) to prevent drift. Residues within 5 Å of the center are free to move.

4. **Force field**: OPLS_2005 or OPLS4 (the force field underlying all Schrödinger simulations).

### 1.2 Grand Canonical Monte Carlo Simulation

The core simulation is **not** a conventional NVT or NPT molecular dynamics simulation. Instead, WaterMap uses GCMC, which is the natural ensemble for an open system exchanging particles with a reservoir — exactly the physical situation of a binding site exchanging water molecules with bulk solvent.

**The GCMC algorithm:**

```
For each Monte Carlo step (total: ~10⁶ steps, 2 ns equivalent):
  1. Choose a random move type with probability:
     - 30%: Translate a randomly selected water molecule
     - 20%: Rotate a randomly selected water molecule  
     - 25%: Insert a water molecule at a random position within the sphere
     - 25%: Delete a randomly selected water molecule
  
  2. Accept/reject based on the Metropolis criterion:
     P(accept) = min(1, exp(−βΔU + B·ΔN))
     
     where:
       β = 1/(k_B·T)
       ΔU = change in potential energy (OPLS force field)
       B = Adams-Broughton parameter: B = β·μ + ln(⟨N⟩/⟨V⟩·Λ³)
       ΔN = change in number of water molecules (+1 for insert, −1 for delete)
       μ = chemical potential of bulk TIP4P water
       Λ = thermal de Broglie wavelength
```

The critical parameter is **B**, the Adams-Broughton parameter. It encodes the chemical potential of bulk water and controls the equilibrium density of the GCMC simulation. B is calibrated so that the water density far from the protein (in the "bulk" region of the sphere) matches the experimental density of liquid water at 300 K and 1 atm (0.033 molecules/Å³).

GCMC is essential — not optional — because:
- The binding site is an **open system**. Water molecules constantly exchange with bulk solvent. An NVT simulation with fixed water count would artificially constrain which waters can enter or leave.
- GCMC naturally samples fluctuating water occupancy. Some positions have water 90% of the time (stable hydration sites); others have water 10% of the time (unfavorable sites).
- The occupancy statistics directly yield the free energy of water binding via IST.

### 1.3 Inhomogeneous Solvation Theory (IST) Analysis

After the GCMC simulation (typically 2 ns, producing 200,000 snapshots at 10 fs intervals), IST converts the trajectory into thermodynamic quantities.

**Step 1: 3D Density Construction**

The oxygen atom positions from all snapshots are binned into a 3D grid (typical spacing: 0.5 Å). The raw density ρ(r) is computed as:

```
ρ(r) = ⟨N(r)⟩ / V_voxel
```

where ⟨N(r)⟩ is the average number of water oxygen atoms in the voxel centered at position r, and V_voxel is the voxel volume.

**Step 2: Hydration Site Identification**

Local maxima in ρ(r) are identified as hydration sites. A site must have:
- ρ(r) > 2× bulk density (ρ_bulk ≈ 0.033 molecules/Å³) — i.e., water is at least twice as likely to be here as in bulk
- The peak must be at least 1.0 Å from any protein heavy atom (to exclude steric overlap)
- Neighboring peaks within 1.0 Å are merged, keeping the higher-density one

A typical binding site yields 15–40 hydration sites.

**Step 3: Thermodynamic Decomposition**

For each hydration site, IST decomposes the free energy of water binding:

**Enthalpy (ΔH_hyd):**
```
ΔH_hyd = E_ww + E_pw

where:
  E_ww = average water-water interaction energy for water at this site
  E_pw = average protein-water interaction energy for water at this site
```

This is computed by averaging the OPLS force field interaction energies over all snapshots where a water molecule occupies the site (defined as oxygen within 0.6 Å of the site center).

A highly favorable ΔH_hyd (−10 to −15 kcal/mol) indicates strong hydrogen bonds to protein backbone or sidechain groups. A near-zero or positive ΔH_hyd indicates poor hydrogen-bonding geometry.

**Entropy (−TΔS_hyd):**

The entropy is computed from the 6D positional-orientational distribution of water at the site:

```
S = S_trans + S_orient

S_trans = −k_B ∫ ρ(r) ln(ρ(r)/ρ_bulk) dr  (translational entropy relative to bulk)
S_orient = −k_B ∫ P(ω) ln(P(ω)/P_bulk(ω)) dω  (orientational entropy relative to bulk)
```

In practice, these integrals are approximated using:
- A Gaussian fit to the 3D spatial distribution for S_trans
- A histogram over Euler angles (binned at 10° intervals) for S_orient

A highly negative −TΔS_hyd (+5 to +15 kcal/mol, destabilizing) indicates the water is highly constrained — both positionally (trapped in a tight pocket) and orientationally (locked into specific hydrogen bonds). This is the "entropic penalty" for water occupancy.

A near-zero −TΔS_hyd indicates the water retains bulk-like freedom at this position.

**Total Free Energy:**
```
ΔG_hyd = ΔH_hyd − TΔS_hyd
```

The sign convention: **negative ΔG_hyd means water binding is favorable** (the water "wants" to be there). **Positive ΔG_hyd means water binding is unfavorable** (the water would rather be in bulk).

### 1.4 Hydration Site Thermodynamic Classes

WaterMap classifies sites based on their thermodynamic signature:

| Class | ΔH_hyd | −TΔS_hyd | ΔG_hyd | Biological meaning | Displacement |
|-------|--------|----------|--------|--------------------|--------------|
| **Deeply trapped** | ≪ 0 (strong H-bonds) | ≫ 0 (highly constrained) | ≪ 0 (−3 to −8 kcal/mol) | Structurally conserved water; essential for protein stability | **Penalized** — strong unfavorable ΔG |
| **Enthalpically favorable** | ≪ 0 | ≈ 0 (bulk-like freedom) | < 0 (−1 to −3 kcal/mol) | Good H-bonds, not entropically trapped | Displaceable if ligand H-bonds are better |
| **Entropically frustrated** | ≈ 0 (poor H-bonds) | ≫ 0 (tight pocket, poor geometry) | > 0 (+0.5 to +5 kcal/mol) | "Unhappy" water; wants to leave | **Rewarded** — favorable ΔG on displacement |
| **Bulk-like** | ≈ 0 | ≈ 0 | ≈ 0 | Effectively bulk solvent | No energetic consequence |

The most critical class for drug design is **entropically frustrated** sites — these are the "magic methyl targets." A ligand that can precisely fill such a site displaces the frustrated water to bulk, releasing its entropic constraint for a net favorable free energy change.

## Part 2: Integration into the Glide WS Scoring Function

### 2.1 The Modified Scoring Function

The Glide WS scoring function extends the base Glide SP/XP function with water-specific terms:

```
Score_WS = Score_base + Σ_i w_i(r_lig) · f_i(ΔG_hyd, overlap) + Score_MMGBSA
```

where:
- **Score_base** is the full Glide SP scoring function (vdW, Coulomb, H-bond, desolvation, hydrophobic enclosure from XP)
- **w_i(r_lig)** is the weight for hydration site *i*, a function of the ligand pose's distance to the site
- **f_i** encodes the water displacement energetics specific to site *i*, based on its ΔG_hyd
- **Score_MMGBSA** is an additional MM-GBSA assessment of the docked pose

### 2.2 Overlap Weight Function

The weight w_i(r_lig) determines how strongly hydration site *i* contributes to the score for a given ligand pose. This uses a distance-dependent switching function:

```
w_i(r_lig) = ∏_{atoms j in site i neighborhood} g(d_ij)

where:
  d_ij = distance from ligand atom j to hydration site center i
  g(d) = smooth switching function:
    g(d) = 1                          for d ≤ R_overlap
    g(d) = cos²(π(d−R_overlap)/2w)   for R_overlap < d ≤ R_overlap + w
    g(d) = 0                          for d > R_overlap + w
    
  R_overlap = sum of ligand atom vdW radius + water vdW radius (~1.6 Å)
  w = switching width (typically 0.5 Å)
```

The product over neighboring atoms means: if *any* ligand heavy atom is close to the hydration site, the site contributes. Multiple close atoms produce a stronger signal.

### 2.3 Displacement Free Energy Function

The function f_i encodes the thermodynamic consequence of ligand overlap with site *i*:

```
f_i(ΔG_hyd, overlap_fraction) = overlap_fraction · g_type(i) · (−ΔG_hyd + ΔG_reorg)

where:
  overlap_fraction = fraction of the site's Gaussian density overlapped by ligand atoms
  g_type(i) = site-type-specific coefficient:
    - Deeply trapped: g_type = 1.0 (full penalty)
    - Enthalpically favorable: g_type = 0.5 (partial penalty)
    - Entropically frustrated: g_type = 1.2 (amplified reward)
    - Bulk-like: g_type = 0.1 (nearly no effect)
  ΔG_reorg = estimated reorganization free energy of the protein/remaining waters
```

The **sign** of −ΔG_hyd means:
- For trapped waters (ΔG_hyd ≪ 0): f_i is **positive** → score penalty (worse docking score)
- For frustrated waters (ΔG_hyd > 0): f_i is **negative** → score reward (better docking score)

The g_type coefficients encode empirical knowledge: displacing a deeply trapped water costs exactly its free energy; displacing a frustrated water releases slightly more than its free energy (due to cooperative solvent network effects); and displacing a bulk-like water is essentially free.

### 2.4 Water-Mediated Interaction Reward

Not all water overlap is displacement. Sometimes the optimal configuration preserves the water as a bridge:

```
Score_water_bridge = Σ_i Σ_j Σ_k w_bridge(d_ij, d_ik, θ_jik) · (−ΔH_hyd(i))

where:
  j = protein hydrogen-bond donor/acceptor
  k = ligand hydrogen-bond donor/acceptor
  i = bridging water site
  d_ij = distance from water site i to protein atom j
  d_ik = distance from water site i to ligand atom k
  θ_jik = hydrogen bond angle at water site i
  w_bridge = scoring weight with ideal geometry at:
    d ≈ 2.8 Å, θ ≈ 180° (linear H-bond) or 120° (bifurcated)
```

A water bridge is rewarded proportionally to the **enthalpic** favorability of the water's position (−ΔH_hyd), not the full free energy. This is because the water remains in place (so the entropic penalty is still "paid"), but the bridging interaction adds a stabilizing enthalpy contribution.

This distinguishes Glide WS from methods that simply add static crystallographic waters: a water bridge is only rewarded if the WaterMap analysis confirms that a water at that position is enthalpically favorable. A bridge through an entropically frustrated position would be penalized, as it forces a water into an unfavorable location.

### 2.5 MM-GBSA Correction Term

After the water-specific terms are computed, Glide WS adds an MM-GBSA assessment:

```
Score_MMGBSA = ΔE_MM + ΔG_GB + ΔG_SA

where:
  ΔE_MM = ΔE_vdW + ΔE_coulomb (OPLS force field, no cutoff)
  ΔG_GB = Generalized Born electrostatic solvation energy (igb=2 or igb=5 model)
  ΔG_SA = Nonpolar solvation (LCPO surface area model: γ·ΔSASA + b)
```

The MM-GBSA term serves as a physics-based "sanity check" on the empirical scoring. It is computed using a single-point energy evaluation of the docked pose, not an MD trajectory (unlike full Nwat-MMGBSA rescoring, which averages over frames). The MM-GBSA energy is added with a small weight (typically 0.1–0.2) to avoid dominating the score.

## Part 3: Computational Cost and Practical Considerations

### 3.1 WaterMap Cost

A typical WaterMap calculation:
- **System**: Protein + ~1,000 TIP4P water molecules in 12 Å sphere
- **GCMC simulation**: 2 ns equivalent ≈ 10⁶ Monte Carlo steps
- **CPU**: ~3–6 hours on 4 cores (Desmond MD engine)
- **GPU**: ~30–60 minutes on a single GPU (Desmond/GPU)
- **IST analysis**: ~5 minutes (post-processing)

The result is stored and reused for all subsequent docking runs against that target — WaterMap is computed **once per target**, not once per ligand.

### 3.2 Docking Cost

Per-ligand Glide WS cost:
- **Conformer generation**: ~2–5 seconds (hybrid RDKit/ConfGen)
- **Docking with water scoring**: ~10–20 seconds per ligand
- **Total**: ~15–30 seconds per ligand

This is ~20–30× slower than Glide SP (~0.5 seconds/ligand). The speed penalty comes from:
1. Water scoring evaluation: For each of ~100–500 poses per ligand, the overlap with ~20–40 hydration sites must be computed.
2. Multiple water configurations: For flexible waters (those near the ligand), multiple water positions/orientations are sampled during pose optimization.
3. MM-GBSA evaluation: A single-point energy minimization (100–200 steps) for the final pose.

### 3.3 When WaterMap Is Worth It

WaterMap analysis should be performed when:
- The target has a well-defined binding site with structural waters visible in the crystal structure
- There are ≥3 conserved water molecules within 4 Å of bound ligands across PDB structures
- The binding site has enclosed cavities where water trapping is likely
- There is known SAR (structure-activity relationship) data that cannot be explained by direct protein-ligand contacts alone
- The target is a "troublesome" system where Glide SP screening produces high false positive rates

WaterMap (and thus Glide WS) is **less useful** when:
- The binding site is shallow, solvent-exposed, and lacks defined water structure (PPI interfaces, for example)
- The target has no available high-resolution crystal structure
- The goal is ultra-high-throughput screening of >100K compounds — use Glide SP and reserve WS for the top hits

## Part 4: Comparison with Alternative Explicit Water Methods

### WScore (Murphy et al., 2016)
Glide WS is the direct successor to WScore. Key comparison:
- **Water sampling**: Both use WaterMap-derived sites. WScore samples water configurations as part of the docking search; Glide WS pre-computes them more efficiently.
- **Scoring**: Both use WaterMap ΔG_hyd. Glide WS adds FEP+ calibration and MM-GBSA terms.
- **Integration**: WScore was a standalone script; Glide WS is fully integrated into the Glide pipeline with its constraint system, grid pre-computation, and post-processing.

### SIEFScore / GIST
Grid Inhomogeneous Solvation Theory (GIST, from the AmberTools suite) computes water thermodynamics on a 3D grid from an NVT MD simulation, similar in concept to WaterMap but using a fixed-cell periodic simulation rather than GCMC. Key differences:
- **WaterMap uses GCMC** (grand canonical ensemble, open system), which naturally samples fluctuating water occupancy. GIST uses NVT (canonical ensemble, closed system), which means the total number of waters is fixed and some sites may be artificially occupied or empty.
- **WaterMap uses IST with a focus on hydration sites** (discrete 3D points). GIST reports continuous grid-based thermodynamic quantities.
- **WaterMap is tightly integrated with Glide WS** through Schrödinger's software ecosystem. GIST is open-source but requires separate rescoring pipelines.

### WaterSwap
WaterSwap (Woods et al., *J. Chem. Theory Comput.* 2015) uses alchemical free energy perturbation to compute the absolute free energy of individual water molecules, providing more rigorous thermodynamics than IST. However, it is ~100× more expensive than WaterMap and not practical for routine use in a docking pipeline.

### 3D-RISM
The 3D Reference Interaction Site Model provides analytical solvation free energies from integral equation theory. Fast (seconds rather than hours) but less accurate than explicit-solvent methods, particularly for highly confined binding sites where the integral equation approximations break down.

## Part 5: Mathematical Innovations That Distinguish WaterMap

### 5.1 GCMC vs. NVT: The Open-System Advantage

The choice of GCMC over NVT for WaterMap is not incidental — it is mathematically necessary for correct water thermodynamics. In an NVT simulation of a binding site sphere:

- The total number of water molecules is fixed
- If a hydration site is unfavorable, it may still be occupied simply because there is "nowhere else" for the water to go within the sphere
- The water density at that site will be artificially high, producing a false "stable hydration site" signal

In GCMC:
- Water molecules enter and leave the sphere based on the chemical potential μ
- Unfavorable sites are naturally depopulated because insertions are rejected and deletions are accepted
- The equilibrium occupancy at each site correctly reflects its free energy relative to bulk

This is the **grand canonical ensemble's ergodic advantage**: it directly samples the correct statistical ensemble for an open system, avoiding the finite-size artifacts that plague closed-system simulations of small volumes.

### 5.2 IST as an Entropy Estimator

Computing the absolute entropy of a liquid from simulation is notoriously difficult. Standard methods (thermodynamic integration, free energy perturbation) are computationally demanding and require a reference state.

IST sidesteps this by computing the **excess** entropy relative to bulk water:

```
ΔS_hyd = S_site − S_bulk

where S_bulk is known from experiment (≈ 16.7 cal/mol·K for TIP4P water at 300 K)
```

The IST approximation assumes that the 6D positional-orientational distribution P(r,ω) is the dominant contributor to the entropy difference. Higher-order correlations (water-water pair entropies) are neglected. This is acceptable because:
- In a protein binding site, the dominant entropy change is the loss of translational/orientational freedom imposed by the protein
- Water-water correlations in the first solvation shell are similar to bulk (the protein replaces the second shell)
- The error from neglecting pair entropies is estimated at ~0.5–1.0 kcal/mol in ΔG — small relative to typical hydration site free energies (range: −8 to +5 kcal/mol)

### 5.3 The Adams-Broughton Parameter Calibration

The Adams-Broughton parameter B in the GCMC acceptance criterion:

```
B = β·μ_excess + ln(ρ_bulk · Λ³)

where:
  μ_excess = excess chemical potential of TIP4P water (computed separately 
             via Widom test-particle insertion in a bulk water simulation)
  ρ_bulk = experimental liquid water density at 300 K, 1 atm
  Λ = h/√(2π·m·k_B·T)  (thermal de Broglie wavelength)
```

This calibration ensures that the GCMC simulation's equilibrium water density in the "bulk" region of the sphere (far from protein) matches experimental density. Without this calibration, the GCMC simulation would converge to an incorrect density and all subsequent IST free energies would be systematically shifted.

## Part 6: Integration into the Broader Research Tree

This WaterMap integration analysis connects to other nodes in the research tree:

- **[../01-docking-evolution/desolvation_thermodynamics/desolvation_thermodynamics.md](../../01-docking-evolution/desolvation_thermodynamics/desolvation_thermodynamics.md)**: The thermodynamic foundations — enthalpy-entropy compensation, the hydrophobic effect — that WaterMap operationalizes.

- **[../01-docking-evolution/water_mediated_binding/water_mediated_binding.md](../../01-docking-evolution/water_mediated_binding/water_mediated_binding.md)**: The biological role of water bridges, conserved water networks, and water displacement — the phenomena WaterMap quantifies.

- **[../03-nwat-mmgb-sa/closest_water_selection/closest_water_selection.md](../../03-nwat-mmgb-sa/closest_water_selection/closest_water_selection.md)**: How Nwat-MMGBSA's nearest-water selection differs from WaterMap's site-based approach.

- **[../04-benchmarks/04-benchmarks.md](../../04-benchmarks/04-benchmarks.md)**: The algorithmic parallels between WaterMap's GCMC sampling and jump-cannon's GPU negative-sampling force layout.

## Key References

1. Young T, Abel R, Kim B, Berne BJ, Friesner RA (2007). "Motifs for Molecular Recognition Exploiting Hydrophobic Enclosure in Protein–Ligand Binding." *Proc. Natl. Acad. Sci.* **104**(3): 808–813. DOI: 10.1073/pnas.0610202104. — The foundational paper showing that water in confined protein cavities has dramatically different thermodynamics than bulk water, and that this drives molecular recognition.

2. Abel R, Young T, Farid R, Berne BJ, Friesner RA (2008). "Role of the Active-Site Solvent in the Thermodynamics of Factor Xa Ligand Binding." *J. Am. Chem. Soc.* **130**(9): 2817–2831. DOI: 10.1021/ja0771033. — The first application of WaterMap (then unnamed) to a drug target, demonstrating that water displacement thermodynamics explain ligand SAR for factor Xa inhibitors.

3. Abel R, Young T, Farid R, Berne BJ, Friesner RA (2012). "WaterMap: Computing Liquids at Realistic, Biological, Nonperiodic Scales with Atomic Detail." Described in the literature as building on the JACS 2008 and PNAS 2007 papers with the formal methodology and software implementation in Schrödinger's suite.

4. Murphy RB et al. (2016). "WScore: A Flexible and Accurate Treatment of Explicit Water Molecules in Ligand–Receptor Docking." *J. Med. Chem.* **59**(9): 4364–4384. DOI: 10.1021/acs.jmedchem.6b00131. — The WScore method that bridges WaterMap thermodynamics to docking scoring, the direct ancestor of Glide WS.

5. Wang L et al. (2015). "Accurate and Reliable Prediction of Relative Ligand Binding Potency in Prospective Drug Discovery by Way of a Modern Free-Energy Calculation Protocol and Force Field." *J. Am. Chem. Soc.* **137**(7): 2695–2703. DOI: 10.1021/ja512751q. — FEP+ methodology used to calibrate the Glide WS scoring function.

6. Schrödinger (2024). "20 Years of Glide: A Legacy of Docking Innovation and the Next Frontier with Glide WS." White Paper. https://www.schrodinger.com/wp-content/uploads/2024/10/24_593_Glide-WS-White-Paper_Mkt_R6-1.pdf — Primary source for Glide WS architecture, performance benchmarks, and the WaterMap-Glide integration methodology.

7. Li Z, Lazaridis T (2007). "Water at Biomolecular Binding Interfaces." *Phys. Chem. Chem. Phys.* **9**(5): 573–581. — Review of water thermodynamics in protein-ligand binding, providing the theoretical framework that WaterMap implements.

8. Hummer G, Rasaiah JC, Noworyta JP (2001). "Water Conduction through the Hydrophobic Channel of a Carbon Nanotube." *Nature* **414**: 188–190. — The grand canonical Monte Carlo approach to confined water that inspired WaterMap's GCMC methodology.
