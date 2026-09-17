# TPT Live Production — Commercial Todo

Source of truth: `spec.txt`. This file tracks commercial/business tasks
only. Engineering/implementation tasks live in `todo.md`.

---

## Pricing (spec 2.1)

- [ ] Validate Standard tier price point (~$1,499) with target buyers
- [ ] Validate Professional tier price point (~$2,999) with target buyers
- [ ] Validate Broadcast tier price point ($4,999+) with target buyers
- [ ] Confirm perpetual-licence model (no recurring cloud infra cost baked
      into core product) as the default commercial model

## Competitive Positioning (spec 2.2)

- [ ] Validate differentiation claims against software switchers (vMix,
      OBS Studio, Wirecast)
- [ ] Validate differentiation claims against hardware switchers (Blackmagic
      ATEM and similar)
- [ ] Validate differentiation claims against lighting consoles (grandMA,
      ChamSys and similar)
- [ ] Validate differentiation claims against Companion-style bridge tools
- [ ] Confirm messaging centers on: native Rust real-time performance,
      unified cue stack (video+audio+lighting), offline-first/no cloud on
      signal path, explicit rehearsal/live safety, built-in failsafe
      behaviour, shared foundation with TPT AV Commissioning/Automation

## Licensing Decision

- [ ] Confirm dual-license choice: MIT OR Apache-2.0
- [ ] Confirm copyright/licensor name: TPT Solutions
- [ ] Decide how dual licensing is represented in marketing/packaging
      materials (README badge, website, purchase terms)
- [ ] Note: creating the actual `LICENSE` file(s) in the repo is tracked in
      `todo.md` Phase 0 — this item is the business/legal decision only

## Packaging Tiers (spec 25)

- [ ] Finalize Standard tier scope: single-operator console, moderate
      source/output count, DMX scene-recall lighting, one control-surface
      protocol, one-machine licence
- [ ] Finalize Professional tier scope: higher source/output counts, richer
      transitions/compositing, multiple control-surface protocols, priority
      updates
- [ ] Finalize Broadcast tier scope: redundancy features, integrator-managed
      deployment support, commercial support
- [ ] Explicitly defer Broadcast-tier redundancy work until customer demand
      is evidenced

## Target Customers & Go-to-Market (spec 2)

- [ ] Identify first customer segment(s) to approach from: live event
      production companies, corporate AV teams, houses of worship, theatres/
      performing-arts venues, broadcast studios, education/lecture-capture
      teams, community/independent broadcasters
- [ ] Recruit a real live-event production team for private beta (final
      step of spec section 28's implementation order — coordinate timing
      with engineering readiness tracked in `todo.md` Phase 12)
- [ ] Define beta success criteria / feedback loop back into MVP scope

## Strategic Boundary Review (spec 26, 29)

- [ ] Periodically confirm the product has not drifted into: a full
      lighting console, a full NLE, a built-in streaming-encoder product, a
      rule-automation authoring tool, or a pre-show installation testing
      tool
- [ ] Confirm public positioning still matches the "Switch / Mix / Cue / Run
      the show live" centre of gravity, not feature-parity chasing against
      vMix/ATEM/dedicated lighting consoles
