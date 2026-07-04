use crate::image_worker::{FilterType, ScaleMode, SlideshowConfig};
use ratatui_image::picker::ProtocolType;
use std::io::{self, IsTerminal};
use std::path::PathBuf;

/// Parsed CLI option arguments.
pub struct CliOptions {
    /// Initial path to display (can be a local file or directory, or cbz archive).
    pub initial_path: Option<PathBuf>,
    /// Scaling filter: nearest, linear, cubic, mitchell, gaussian, lanczos, hamming.
    pub filter: FilterType,
    /// Terminal graphics protocol option: kitty, sixel, halfblocks, iterm2.
    pub protocol: Option<ProtocolType>,
    /// Image scale mode: none, actual, shrink, full, crop.
    pub scale: ScaleMode,
    /// Delay duration in seconds for slideshow transitions.
    pub slideshow: Option<SlideshowConfig>,
    /// If true, validates files by checking their magic bytes instead of extensions.
    pub check_magic: bool,
    /// If true, disables EXIF thumbnail loading/display entirely.
    pub no_thumbnail: bool,
}

/// Reads piped file paths from standard input when stdin is not a terminal,
/// then hijacks/re-routes stdin to `/dev/tty` so keyboard TUI controls work.
pub fn read_piped_stdin() -> Vec<PathBuf> {
    let mut piped_files = Vec::new();
    let is_piped = !io::stdin().is_terminal();
    if is_piped {
        use std::io::BufRead;
        let stdin = io::stdin();
        let mut handle = stdin.lock();
        let mut buf = Vec::new();

        // Read line-by-line using raw bytes to support non-UTF-8 unix paths
        while let Ok(n) = handle.read_until(b'\n', &mut buf) {
            if n == 0 {
                break;
            }
            // Trim trailing \n and \r
            let mut len = buf.len();
            while len > 0 && (buf[len - 1] == b'\n' || buf[len - 1] == b'\r') {
                len -= 1;
            }
            buf.truncate(len);

            if !buf.is_empty() {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStringExt;
                    let path = PathBuf::from(std::ffi::OsString::from_vec(buf.clone()));
                    if path.exists() && path.is_file() {
                        piped_files.push(path);
                    }
                }
                #[cfg(not(unix))]
                {
                    if let Ok(s) = std::str::from_utf8(&buf) {
                        let path = PathBuf::from(s);
                        if path.exists() && path.is_file() {
                            piped_files.push(path);
                        }
                    }
                }
            }
            buf.clear();
        }

        // Reopen stdin from /dev/tty so crossterm can read keyboard inputs!
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            if let Ok(tty) = std::fs::OpenOptions::new().read(true).open("/dev/tty") {
                let fd = tty.as_raw_fd();
                unsafe {
                    let _ = libc::dup2(fd, libc::STDIN_FILENO);
                }
            }
        }
    }
    piped_files
}

/// Helper function to retrieve flag value and avoid argument hijacking.
fn get_arg(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    if *i + 1 < args.len() {
        let val = &args[*i + 1];
        if val.starts_with('-') {
            return Err(format!(
                "Error: Argument for {} cannot be another option ({})",
                flag, val
            ));
        }
        let res = val.clone();
        *i += 2;
        Ok(res)
    } else {
        Err(format!("Error: Option {} requires an argument", flag))
    }
}

/// Parses the command-line options from environment arguments.
pub fn parse_cli_args() -> Result<CliOptions, String> {
    let args: Vec<String> = std::env::args().collect();
    let mut initial_path: Option<PathBuf> = None;
    let mut filter = FilterType::Nearest;
    let mut protocol = None;
    let mut scale = ScaleMode::Shrink;
    let mut slideshow = None;
    let mut check_magic = false;
    let mut no_thumbnail = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--filter" | "-f" => {
                let flag = args[i].clone();
                let val = get_arg(&args, &mut i, &flag)?;
                filter = match val.to_lowercase().as_str() {
                    "nearest" => FilterType::Nearest,
                    "linear" => FilterType::Triangle,
                    "cubic" => FilterType::CatmullRom,
                    "mitchell" => FilterType::Mitchell,
                    "gaussian" => FilterType::Gaussian,
                    "lanczos" => FilterType::Lanczos3,
                    "hamming" => FilterType::Hamming,
                    other => {
                        return Err(format!(
                            "Error: Unknown filter '{}'. Choose from: nearest, linear, cubic, mitchell, gaussian, lanczos, hamming",
                            other
                        ));
                    }
                };
            }
            "--protocol" | "-p" => {
                let flag = args[i].clone();
                let val = get_arg(&args, &mut i, &flag)?;
                let proto = match val.to_lowercase().as_str() {
                    "kitty" => ProtocolType::Kitty,
                    "sixel" => ProtocolType::Sixel,
                    "halfblocks" | "halfblock" => ProtocolType::Halfblocks,
                    "iterm2" => ProtocolType::Iterm2,
                    other => {
                        return Err(format!(
                            "Error: Unknown protocol '{}'. Choose from: kitty, sixel, halfblocks, iterm2",
                            other
                        ));
                    }
                };
                protocol = Some(proto);
            }
            "--scale" | "-s" => {
                let flag = args[i].clone();
                let val = get_arg(&args, &mut i, &flag)?;
                scale = match val.to_lowercase().as_str() {
                    "none" | "actual" => ScaleMode::None,
                    "shrink" => ScaleMode::Shrink,
                    "full" | "fit" => ScaleMode::Full,
                    "crop" => ScaleMode::Crop,
                    other => {
                        return Err(format!(
                            "Error: Unknown scale mode '{}'. Choose from: none, actual, shrink, full, crop",
                            other
                        ));
                    }
                };
            }
            "--slideshow" | "-t" => {
                let flag = args[i].clone();
                let val = get_arg(&args, &mut i, &flag)?;
                if let Ok(config) = val.parse::<SlideshowConfig>() {
                    slideshow = Some(config);
                } else {
                    return Err(
                        "Error: -t / --slideshow requires a positive integer argument".into(),
                    );
                }
            }
            "--check-magic" | "-m" => {
                check_magic = true;
                i += 1;
            }
            "--no-thumbnail" => {
                no_thumbnail = true;
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
                println!(
                    "      --no-thumbnail         Disable low-res EXIF thumbnail placeholder loading"
                );
                println!("  -h, --help                 Show this help menu");
                std::process::exit(0);
            }
            val => {
                if val.starts_with('-') {
                    return Err(format!("Error: Unknown option '{}'", val));
                }
                if let Some(ref path) = initial_path {
                    return Err(format!(
                        "Error: Only a single path argument is supported, but multiple paths were provided: '{}' and '{}'",
                        path.display(),
                        val
                    ));
                }
                initial_path = Some(PathBuf::from(val));
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
        no_thumbnail,
    })
}
