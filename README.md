# imv-tui

`imv-tui` is a fast, keyboard-driven terminal image viewer written in Rust. Heavily inspired by the native `imv` image viewer, it enables zooming, panning, rotation, and directory-based navigation directly inside your terminal window.

![imv-tui Screenshot](https://p0.pe/my-git-repos/imv-tui.git/plain/screenshots/01.png)

It is built on top of [`ratatui`](https://crates.io/crates/ratatui) and [`ratatui-image`](https://crates.io/crates/ratatui-image).

______________________________________________________________________

## Features

- **Directory Navigation**: Open an image, and it automatically scans the directory for all sibling files, allowing you to cycle through them effortlessly.
- **Manga/Comic CBZ & ZIP Support**: Open and flip through compressed `.cbz` and `.zip` archives directly. Pages are sorted alphabetically and loaded asynchronously.
- **Image Manipulation Adjustments**: Adjust brightness and contrast in real-time. Operations are processed asynchronously on background threads for fluid performance.
- **Header-Based Magic Byte Verification**: Intelligently identifies images and zip files by reading their headers, allowing images with missing or incorrect file extensions to load perfectly.
- **Decoded Format Nerd Font Icons**: Intelligently parses magic bytes and matches the decoded format with clean Material Nerd Font icons in the status bar (e.g., JPEG, PNG, GIF) to easily spot files with incorrect extensions.
- **Three-Section Info Bar**: Formats status metadata at the bottom of the screen into three cleanly aligned sections: image sequence & dimensions on the left, scaling/panning parameters centered in the middle, and command palette discovery shortcut on the right.
- **Smart Aspect-Ratio Padding Compression**: When zoomed out, images sit centered with empty border spaces (padding). As you zoom in, the borders automatically shrink and disappear, scaling the image to fully cover the terminal space.
- **Pixel-Perfect Deep Zooming**: Supports zooming beyond a 1:1 pixel scale (up to 10000%) with clean Nearest Neighbor scaling—perfect for inspection and pixel-art view.
- **Predefined Level Jumps**: Instantly snap zoom levels using `I` and `O` through a calculated sequence of target scales: Shrink to Fit, Fit View, Crop to Fill, 1:1, 2:1, and 4:1.
- **In-Memory Rotation**: Rotate vertical or misaligned images clockwise and counter-clockwise in-memory (does not modify files on disk).
- **Centering Layout**: Fits and centers images horizontally and vertically when they are smaller than the terminal size.
- **Interactive Command Palette & File Search**: Press `:` to trigger the command palette or `f` to search for files, utilizing the high-performance `nucleo` fuzzy matching engine to rank and display the best candidates first.
- **Dynamic Parameter Value Prompts**: Adjust brightness, contrast, or jump directly to a specific image index (via `Go to Image`, `Set Brightness`, and `Set Contrast` commands in the command palette) using absolute numbers or relative offsets (e.g. `+10` or `-5`).
- **Slideshow Mode**: Play a slideshow of images with configurable delays, adjustable dynamically via keyboard shortcuts (`t`/`T`), CLI parameters, or from the command palette.
- **Image Classification & Filtered Views**: Flag images as Picks (⭐), Rejects (❌), or Unflagged (⚪) in memory. Filter the navigation queue and file list dynamically using five view modes (Unflagged + Picks, Unflagged Only, Picks Only, Rejects Only, All Files) to easily sort, select, or hide images.
- **Graphics & Fallbacks**: Auto-detects terminal capabilities. Uses high-performance Kitty graphics protocol or Sixel if supported, falling back gracefully to ANSI **Half-blocks** on standard terminals.

______________________________________________________________________

## Keyboard Shortcuts

| Action                              | Primary Key | Alternative Keys            |
| :---------------------------------- | :---------- | :-------------------------- |
| **Quit**                            | `q`         | `Esc`                       |
| **Next Image**                      | `n`         | `Space` / `]`               |
| **Previous Image**                  | `p`         | `Backspace` / `[`           |
| **Zoom In**                         | `i`         | `+` / `=` / Mouse Scroll Up |
| **Zoom In (predefined levels)**     | `I`         |                             |
| **Zoom Out**                        | `o`         | `-` / Mouse Scroll Down     |
| **Zoom Out (predefined levels)**    | `O`         |                             |
| **Actual Size (100% Zoom)**         | `a`         |                             |
| **Reset View (Fit Screen)**         | `r`         |                             |
| **Rotate Clockwise (90°)**          | `e`         | `R` / `>`                   |
| **Rotate Counter-Clockwise (90°)**  | `E`         | `<`                         |
| **Brightness Increase / Decrease**  | `b` / `B`   |                             |
| **Contrast Increase / Decrease**    | `c` / `C`   |                             |
| **Slideshow Increase / Decrease**   | `t` / `T`   |                             |
| **Cycle Image Scaling Filter**      | `S`         |                             |
| **Cycle Image Scaling Mode**        | `s`         |                             |
| **Pan Left / Right**                | `h` / `l`   | `Left` / `Right Arrow`      |
| **Pan Up / Down**                   | `k` / `j`   | `Up` / `Down Arrow`         |
| **Show Help / Command Palette**     | `?` / `:`   | `/`                         |
| **File Search**                     | `f`         |                             |
| **Toggle Thumbnail Display**        | `m`         |                             |
| **Mark Image as Pick**              | `z`         |                             |
| **Mark Image as Reject**            | `x`         |                             |
| **Unflag Image**                    | `u`         |                             |
| **Cycle View Filter**               | `v`         |                             |
| **Jump to View: Unflagged + Picks** | `1`         |                             |
| **Jump to View: Unflagged Only**    | `2`         |                             |
| **Jump to View: Picks Only**        | `3`         |                             |
| **Jump to View: Rejects Only**      | `4`         |                             |
| **Jump to View: All Files**         | `5`         |                             |

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

### 4. Background Thread Loader & Sliding Window Cache

To ensure stutter-free navigation under high key-repeat rates:

- **Persistent Image Loader**: Offloads image decoding to a dedicated background thread. Image loader requests are sequenced; during fast navigation, the loader thread coalesces pending requests, processes the active viewport first, and discards stale sequence requests to prevent thread and disk contention.
- **$2N+1$ Sliding Window Cache**: Caches the active image alongside 2 preceding and 2 succeeding images ($N=2$) in CPU memory. Prunes out-of-bounds cache items dynamically on navigation, providing instantaneous response times when cycling back and forth across pictures.

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

- `-f, --filter <nearest|linear|cubic|mitchell|gaussian|lanczos|hamming>`: Set the initial image scaling filter (defaults to `nearest`).
- `-s, --scale <none|actual|shrink|full|crop>`: Set the initial image scaling mode (defaults to `shrink`). `actual` maps to `none`, and `fit` maps to `full`.
- `-p, --protocol <kitty|sixel|halfblocks|iterm2>`: Force a specific terminal graphics protocol (bypassing auto-detection). `halfblock` maps to `halfblocks`.
- `-t, --slideshow <seconds>`: Start the slideshow with the given delay in seconds.
- `-m, --check-magic`: Check file magic bytes on startup (slower on network drives).
- `--no-thumbnail`: Disable low-res EXIF thumbnail placeholder loading entirely.
- `-h, --help`: Displays the help menu outlining CLI usage and flags.

______________________________________________________________________

## Attribution

This project was built and generated with the help of an AI Large Language Models (LLMs).
