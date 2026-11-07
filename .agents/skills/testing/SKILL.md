---
name: testing
description: Guidelines for writing unit tests, mocking async channels, and validating clamping invariants.
---

# Testing & Mocking Invariants

### 1. Invariant Validation Tests

- Custom value types (`Brightness`, `Contrast`, `PanOffset`, `CropBox`) must enforce clamping bounds.
- Always write inline `#[cfg(test)]` modules that assert limits (e.g. contrast clamped to `[-255.0, 255.0]`, brightness to `[-255, 255]`).
- Verify NaN inputs are safely rejected/ignored without panicking.

### 2. Async Channel Mocking

- When writing tests for state transitions or directory refreshes, mock the background loading channels (`loader_tx`, `response_rx`).
- Pre-populate the cache with dummy `CachedImage` values and simulate message arrival to test cache-hit paths synchronously.
- Avoid sleeping in tests; utilize non-blocking polling or synchronized mocks to verify event loops.

### 3. Code Coverage (cargo-tarpaulin)

- Code coverage can be measured by running `cargo tarpaulin`.
- It is configured via `.tarpaulin.toml` (`engine = "Ptrace"`, `skip-clean = true`, `no-dead-code = true`) and `[profile.test] opt-level = 0` in `Cargo.toml` so test binaries compile at `opt-level = 0` for clean `ptrace` breakpoint alignment while dependencies remain optimized at `opt-level = 3`.
- Tests should focus on high-value behavioral logic, event handling, state transitions, and boundary math rather than trivial getters/setters.
