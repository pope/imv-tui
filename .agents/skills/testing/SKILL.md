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
