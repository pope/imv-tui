use crate::config::{InfoBarPosition, SlideshowState};
use crate::imaging::{FilterType, ScaleMode};
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
    pub slideshow: Option<SlideshowState>,
    /// If true, validates files by checking their magic bytes instead of extensions.
    pub check_magic: bool,
    /// If true, disables EXIF thumbnail loading/display entirely.
    pub no_thumbnail: bool,
    /// Path to a file to import classification states from.
    pub import_path: Option<PathBuf>,
    /// Path to a file to export classification states to on exit.
    pub export_path: Option<PathBuf>,
    /// Position of the info bar (top, bottom, none).
    pub infobar: InfoBarPosition,
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

pub fn parse_cli_args() -> Result<CliOptions, String> {
    parse_cli_args_from(std::env::args())
}

/// Parses the command-line options from any string argument iterator.
pub fn parse_cli_args_from<I>(args_iter: I) -> Result<CliOptions, String>
where
    I: IntoIterator<Item = String>,
{
    let args: Vec<String> = args_iter.into_iter().collect();
    let mut initial_path: Option<PathBuf> = None;
    let mut filter = FilterType::Nearest;
    let mut protocol = None;
    let mut scale = ScaleMode::Shrink;
    let mut slideshow = None;
    let mut check_magic = false;
    let mut no_thumbnail = false;
    let mut import_path = None;
    let mut export_path = None;
    let mut sync_path = None;
    let mut infobar = InfoBarPosition::Bottom;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--filter" | "-f" => {
                let flag = args[i].clone();
                let val = get_arg(&args, &mut i, &flag)?;
                filter = match val.to_lowercase().as_str() {
                    "nearest" => FilterType::Nearest,
                    "linear" => FilterType::Linear,
                    "cubic" => FilterType::Cubic,
                    "mitchell" => FilterType::Mitchell,
                    "gaussian" => FilterType::Gaussian,
                    "lanczos" => FilterType::Lanczos,
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
                if let Ok(config) = val.parse::<SlideshowState>() {
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
            "--infobar" => {
                let flag = args[i].clone();
                let val = get_arg(&args, &mut i, &flag)?;
                infobar = match val.to_lowercase().as_str() {
                    "top" => InfoBarPosition::Top,
                    "bottom" => InfoBarPosition::Bottom,
                    "none" => InfoBarPosition::None,
                    other => {
                        return Err(format!(
                            "Error: Unknown infobar position '{}'. Choose from: top, bottom, none",
                            other
                        ));
                    }
                };
            }
            "--sync" | "-r" => {
                let flag = args[i].clone();
                let val = get_arg(&args, &mut i, &flag)?;
                sync_path = Some(PathBuf::from(val));
            }
            "--import" | "-i" => {
                let flag = args[i].clone();
                let val = get_arg(&args, &mut i, &flag)?;
                import_path = Some(PathBuf::from(val));
            }
            "--export" | "-o" => {
                let flag = args[i].clone();
                let val = get_arg(&args, &mut i, &flag)?;
                export_path = Some(PathBuf::from(val));
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
                println!(
                    "  -i, --import <file>        Import image classification/flagged states from a file (.json or prefix text)"
                );
                println!(
                    "  -o, --export <file>        Export image classification/flagged states to a file on exit (.json or prefix text)"
                );
                println!(
                    "  -r, --sync <file>          Sync image classification/flagged states with a file (imports on startup, exports on exit)"
                );
                println!(
                    "      --infobar <position>   Position of the info bar: top, bottom, none (defaults to bottom)"
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
    if sync_path.is_some() && (import_path.is_some() || export_path.is_some()) {
        return Err(
            "Error: Option --sync (-r) cannot be used together with --import (-i) or --export (-o)"
                .to_string(),
        );
    }

    if let Some(path) = sync_path {
        import_path = Some(path.clone());
        export_path = Some(path);
    }

    Ok(CliOptions {
        initial_path,
        filter,
        protocol,
        scale,
        slideshow,
        check_magic,
        no_thumbnail,
        import_path,
        export_path,
        infobar,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_exclusion() {
        // sync and import together should fail
        let args1 = vec![
            "imv-tui".to_string(),
            "-r".to_string(),
            "sync.json".to_string(),
            "-i".to_string(),
            "import.json".to_string(),
        ];
        assert!(parse_cli_args_from(args1).is_err());

        // sync and export together should fail
        let args2 = vec![
            "imv-tui".to_string(),
            "--sync".to_string(),
            "sync.json".to_string(),
            "-o".to_string(),
            "export.json".to_string(),
        ];
        assert!(parse_cli_args_from(args2).is_err());

        // sync alone should succeed
        let args3 = vec![
            "imv-tui".to_string(),
            "-r".to_string(),
            "sync.json".to_string(),
        ];
        let opts = parse_cli_args_from(args3).unwrap();
        assert_eq!(opts.import_path, Some(PathBuf::from("sync.json")));
        assert_eq!(opts.export_path, Some(PathBuf::from("sync.json")));
    }
}
