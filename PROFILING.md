# Profiling Guide for `imv-tui`

To identify bottlenecks in a Rust terminal application like `imv-tui`, we need to compile it in release mode with debug symbols and use a sampling profiler.

This guide covers:

1. **Cargo Configuration Changes**
2. **Configuring Linux Kernel Permissions**
3. **Using Samply (Recommended)**
4. **Using Cargo Flamegraph**
5. **Quick In-App Micro-Benchmarking**

______________________________________________________________________

## 1. Cargo Configuration

Profiling a debug build (`cargo build`) is misleading because compiler optimizations are disabled, resulting in slow operations that wouldn't exist in production.

To profile optimized code, we compile in **release mode** but instruct Cargo to preserve **debug symbols** (line numbers and function names).

We have updated [Cargo.toml](Cargo.toml) to include:

```toml
[profile.release]
debug = true
```

> [!NOTE]
> Setting `debug = true` keeps full debug symbols. If binary size is a concern, you can use `debug = 1` to include line tables only, which is sufficient for backtraces and basic profiling.

______________________________________________________________________

## 2. Linux Kernel Permissions (`perf`)

Both `samply` and `flamegraph` rely on the Linux kernel `perf` subsystem. By default, Linux restricts unprivileged users from accessing performance counters.

To allow profiling without root permissions, you should adjust the `perf_event_paranoid` level:

```bash
# Allow unprivileged users to use perf events (until reboot)
sudo sysctl -w kernel.perf_event_paranoid=1
```

To make this change persistent across reboots, add it to `/etc/sysctl.d/`:

```bash
echo "kernel.perf_event_paranoid=1" | sudo tee /etc/sysctl.d/99-perf-profiling.conf
```

______________________________________________________________________

## 3. Profiling with Samply (Recommended)

`samply` is an extremely user-friendly sampling profiler for Linux and macOS. It records execution and runs a local web server displaying the results in the interactive [Firefox Profiler](https://profiler.firefox.com/) interface.

### Step-by-Step Instructions:

1. **Install / Enable `samply`**:
   Since `samply` is configured in [flake.nix](flake.nix), it is automatically available inside your Nix dev shell (`nix develop`).

   *(If not using the Nix shell, install it via: `cargo install samply`)*

2. **Build your project in release mode**:

   ```bash
   cargo build --release
   ```

3. **Run `samply` with your application**:
   Provide a sample image file to load, for example:

   ```bash
   samply record ./target/release/imv-tui <path-to-image>
   ```

4. **Interact with the TUI**:
   Perform actions where you suspect bottlenecks (e.g., zoom in/out, pan, rotate, switch images).

5. **Exit the application**:
   Press `q` to quit `imv-tui`.

6. **Analyze the profile**:
   `samply` will print a URL to your console (usually starting with `http://127.0.0.1:xxxx`) and try to open it in your browser. The page displays a rich visualization including a timeline of CPU activity, call stacks, flame graphs, and a breakdown of time spent per function.

______________________________________________________________________

## 4. Profiling with Cargo Flamegraph

If you prefer generating static SVG files, you can use `cargo-flamegraph`, which uses `perf` to record call stacks and produces a visualization where wider boxes represent functions taking more CPU time.

### Step-by-Step Instructions:

1. **Install / Enable `flamegraph`**:
   Since `cargo-flamegraph` is configured in [flake.nix](flake.nix), it is automatically available inside your Nix dev shell (`nix develop`).

   *(If not using the Nix shell, install it via: `cargo install flamegraph`)*

2. **Run the profiler**:
   `cargo-flamegraph` compiles the code automatically (using your release profile) and profiles it:

   ```bash
   cargo flamegraph --bin imv-tui -- <path-to-image>
   ```

3. **Interact and Exit**:
   Spend some time panning and zooming, then exit with `q`.

4. **Open the Flamegraph**:
   Open the generated `flamegraph.svg` file in any web browser:

   ```bash
   xdg-open flamegraph.svg
   ```

______________________________________________________________________

## 5. Inline Timing (Micro-benchmarking)

If you want to measure specific code blocks (like image decoding or resizing) directly in the console logs or system prints, you can use standard library timers.

For example, to profile [update_protocol](src/main.rs#L323-L441):

```rust
let start = std::time::Instant::now();

// ... execution block ...

let elapsed = start.elapsed();
// Log the time (e.g., write to a file or standard error)
eprintln!("update_protocol took: {:?}", elapsed);
```

> [!WARNING]
> Because `imv-tui` is a terminal UI application, writing timing logs directly to `stdout` or `stderr` will corrupt the TUI display. Always redirect stderr to a log file:
>
> ```bash
> ./target/release/imv-tui <image> 2> timing.log
> ```
