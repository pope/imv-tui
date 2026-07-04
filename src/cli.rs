use std::io::{self, IsTerminal};
use std::path::PathBuf;
use crate::image_worker::SlideshowConfig;

/// Parsed CLI option arguments.
pub struct CliOptions {
    /// Initial path to display (can be a local file or directory, or cbz archive).
    pub initial_path: Option<PathBuf>,
    /// Scaling filter: nearest, linear, cubic, mitchell, gaussian, lanczos, hamming.
    pub filter: Option<String>,
    /// Terminal graphics protocol option: kitty, sixel, halfblocks, iterm2.
    pub protocol: Option<String>,
    /// Image scale mode: none, actual, shrink, full, crop.
    pub scale: Option<String>,
    /// Delay duration in seconds for slideshow transitions.
    pub slideshow: Option<SlideshowConfig>,
    /// If true, validates files by checking their magic bytes instead of extensions.
    pub check_magic: bool,
}

/// Reads piped file paths from standard input when stdin is not a terminal,
/// then hijacks/re-routes stdin to `/dev/tty` so keyboard TUI controls work.
pub fn read_piped_stdin() -> Vec<PathBuf> {
    let mut piped_files = Vec::new();
    let is_piped = !io::stdin().is_terminal();
    if is_piped {
        use std::io::BufRead;
        let stdin = io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            let path = PathBuf::from(line.trim());
            if path.exists() && path.is_file() {
                piped_files.push(path);
            }
        }

        // Reopen stdin from /dev/tty so crossterm can read keyboard inputs!
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            if let Ok(tty) = std::fs::OpenOptions::new().read(true).open("/dev/tty") {
                let fd = tty.as_raw_fd();
                unsafe {
                    libc::dup2(fd, libc::STDIN_FILENO);
                }
            }
        }
    }
    piped_files
}

/// Parses the command-line options from environment arguments.
pub fn parse_cli_args() -> Result<CliOptions, String> {
    let args: Vec<String> = std::env::args().collect();
    let mut initial_path = None;
    let mut filter = None;
    let mut protocol = None;
    let mut scale = None;
    let mut slideshow = None;
    let mut check_magic = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--filter" | "-f" => {
                if i + 1 < args.len() {
                    filter = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    return Err(
                        "Error: --filter / -f requires an argument (nearest, linear, cubic, mitchell, gaussian, lanczos, hamming)".into()
                    );
                }
            }
            "--protocol" | "-p" => {
                if i + 1 < args.len() {
                    protocol = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    return Err(
                        "Error: --protocol / -p requires an argument (kitty, sixel, halfblocks, iterm2)".into()
                    );
                }
            }
            "--scale" | "-s" => {
                if i + 1 < args.len() {
                    scale = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    return Err(
                        "Error: --scale / -s requires an argument (none, actual, shrink, full, crop)".into()
                    );
                }
            }
            "--slideshow" | "-t" => {
                if i + 1 < args.len() {
                    if let Ok(config) = args[i + 1].parse::<SlideshowConfig>() {
                        slideshow = Some(config);
                    } else {
                        return Err("Error: -t / --slideshow requires a positive integer argument".into());
                    }
                    i += 2;
                } else {
                    return Err("Error: -t / --slideshow requires an argument".into());
                }
            }
            "--check-magic" | "-m" => {
                check_magic = true;
                i += 1;
            }
            "--help" | "-h" => {
                println!("imv-tui: A fast keyboard-driven terminal image viewer");
                println!();
                println!("Usage: imv-tui [path] [options]");
                println!();
                println!("Options:");
                println!(
                    "  -f, --filter <filter>      Initial image scaling filter: nearest, linear, cubic, mitchell, gaussian, lanczos, hamming"
                );
                println!(
                    "  -p, --protocol <protocol>  Force terminal graphics protocol: kitty, sixel, halfblocks, iterm2"
                );
                println!(
                    "  -s, --scale <mode>         Initial image scaling mode: none, actual, shrink, full, crop (defaults to shrink)"
                );
                println!(
                    "  -t, --slideshow <seconds>  Start the slideshow with the given delay in seconds"
                );
                println!(
                    "  -m, --check-magic          Check file magic bytes on startup (slower on network drives)"
                );
                println!("  -h, --help                 Show this help menu");
                std::process::exit(0);
            }
            val => {
                if initial_path.is_none() {
                    initial_path = Some(PathBuf::from(val));
                }
                i += 1;
            }
        }
    }

    Ok(CliOptions {
        initial_path,
        filter,
        protocol,
        scale,
        slideshow,
        check_magic,
    })
}
