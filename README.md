# imv-tui

`imv-tui` is a fast, keyboard-driven terminal image viewer written in Rust. Heavily inspired by the native `imv` image viewer, it enables zooming, panning, rotation, and directory-based navigation directly inside your terminal window.

It is built on top of [`ratatui`](https://crates.io/crates/ratatui) and [`ratatui-image`](https://crates.io/crates/ratatui-image).

______________________________________________________________________

## Features

- **Directory Navigation**: Open an image, and it automatically scans the directory for all sibling files, allowing you to cycle through them effortlessly.
- **Manga/Comic CBZ & ZIP Support**: Open and flip through compressed `.cbz` and `.zip` archives directly. Pages are sorted alphabetically and loaded asynchronously.
- **Image Manipulation Adjustments**: Adjust brightness and contrast in real-time. Operations are processed asynchronously on background threads for fluid performance.
- **Header-Based Magic Byte Verification**: Intelligently identifies images and zip files by reading their headers, allowing images with missing or incorrect file extensions to load perfectly.
- **Smart Aspect-Ratio Padding Compression**: When zoomed out, images sit centered with empty border spaces (padding). As you zoom in, the borders automatically shrink and disappear, scaling the image to fully cover the terminal space.
- **Pixel-Perfect Deep Zooming**: Supports zooming beyond a 1:1 pixel scale (up to 10000%) with clean Nearest Neighbor scaling—perfect for inspection and pixel-art view.
- **In-Memory Rotation**: Rotate vertical or misaligned images clockwise and counter-clockwise in-memory (does not modify files on disk).
- **Centering Layout**: Fits and centers images horizontally and vertically when they are smaller than the terminal size.
- **Graphics & Fallbacks**: Auto-detects terminal capabilities. Uses high-performance Kitty graphics protocol or Sixel if supported, falling back gracefully to ANSI **Half-blocks** on standard terminals.

______________________________________________________________________

## Keyboard Shortcuts

| Action                             | Primary Key | Alternative Keys            |
| :--------------------------------- | :---------- | :-------------------------- |
| **Quit**                           | `q`         | `Esc`                       |
| **Next Image**                     | `n`         | `Space` / `]`               |
| **Previous Image**                 | `p`         | `Backspace` / `[`           |
| **Zoom In**                        | `i`         | `+` / `=` / Mouse Scroll Up |
| **Zoom Out**                       | `o`         | `-` / Mouse Scroll Down     |
| **Actual Size (100% Zoom)**        | `a`         |                             |
| **Reset View (Fit Screen)**        | `r`         |                             |
| **Rotate Clockwise (90°)**         | `e`         | `R` / `>`                   |
| **Rotate Counter-Clockwise (90°)** | `E`         | `<`                         |
| **Brightness Increase / Decrease** | `b` / `B`   |                             |
| **Contrast Increase / Decrease**   | `c` / `C`   |                             |
| **Pan Left / Right**               | `h` / `l`   | `Left` / `Right Arrow`      |
| **Pan Up / Down**                  | `k` / `j`   | `Up` / `Down Arrow`         |
| **Toggle Help Screen**             | `?`         |                             |
| **Command Palette**                | `:`         |                             |
| **File Search**                    | `f`         |                             |

______________________________________________________________________

## How It Works Under the Hood

### 1. Unified Aspect-Ratio Scaling & Cropping

Rather than sending massive raw images to the terminal graphics protocol and leaving scaling to terminal emulators, `imv-tui` processes pixels in-memory using `image`:

- It maps the target terminal widget dimensions (converted to pixel sizes based on terminal font sizes) and computes a fit-scale factor `s`.
- For any zoom level, it generates a **crop window** dynamically mapped to the target terminal's aspect ratio.
- If the crop box extends outside the image boundaries (due to zoom-out or panning past boundaries), `imv-tui` crops the visible intersection, resizes it, and overlays it onto a screen-resolution canvas. This naturally creates centering padding or off-screen margins while keeping memory usage extremely low.

### 2. Fast Nearest Neighbor Resizing

When zoomed in, the cropped sub-image is scaled in memory to target screen pixels using a fast `Nearest Neighbor` filter. This has two key advantages:

- It bypasses terminal graphics protocol limitations for image upscaling, enabling zoom levels up to 10000%.
- It renders sharp, pixel-perfect scaling rather than blurry linear scaling.
- The resizing overhead is less than `1ms`, ensuring high frame rates during panning and zooming.

### 3. Non-Destructive Rotation

The rotation keys (`e` and `E`) apply 90° clockwise/counter-clockwise operations directly to the image buffer in memory. The layout and cropping dimensions are recalculated on the fly to support vertical view orientations.

______________________________________________________________________

## Building and Running

Ensure you have Rust and Cargo installed.

```bash
# Clone the repository
git clone https://github.com/yourusername/imv-tui.git
cd imv-tui

# Build in release mode
cargo build --release

# Or build using Nix Flakes
nix build

# Run on a file, directory, or CBZ comic archive
./target/release/imv-tui <path-to-image-or-directory-or-cbz>

# Run with a specific starting filter (nearest, linear, cubic, gaussian, lanczos)
./target/release/imv-tui <path-to-image> --filter cubic

# Pipe a list of file paths from another command (like fd or find) via stdin
fd -e png -e jpg . ~/Pictures | ./target/release/imv-tui
```

If no path is specified, it scans and opens images from the current directory (`.`).

### Command Line Options

- `-f, --filter <nearest|linear|cubic|gaussian|lanczos>`: Set the initial image scaling filter (defaults to `nearest`).
- `-h, --help`: Displays the help menu outlining CLI usage and flags.

______________________________________________________________________

## Attribution

This project was built and generated with the help of an AI Large Language Models (LLMs).
