pub use super::{CompressionPolicy, ProtocolConfig};
use crate::config::{InfoBarPosition, SlideshowState};
use crate::imaging::{FilterType, ProtocolType, ScaleMode};
use std::io::{self, IsTerminal};
use std::path::PathBuf;

/// Parsed CLI option arguments.
pub struct CliOptions {
    /// Initial path to display (can be a local file or directory, or cbz archive).
    pub initial_path: Option<PathBuf>,
    /// Scaling filter: nearest, box, linear, mitchell, lanczos.
    pub filter: FilterType,
    /// Terminal graphics protocol configuration.
    pub protocol: ProtocolConfig,
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
    /// If true, scans directories recursively.
    pub recursive: bool,
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
    }
    piped_files
}

/// Reopens STDIN from `/dev/tty` so crossterm can read interactive keyboard inputs in piped mode.
pub fn reopen_tty_stdin() -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let tty = std::fs::OpenOptions::new()
            .read(true)
            .open("/dev/tty")
            .map_err(|e| {
                format!("Error: Failed to reopen /dev/tty for interactive input in piped mode: {e}")
            })?;
        let fd = tty.as_raw_fd();
        let res = unsafe { libc::dup2(fd, libc::STDIN_FILENO) };
        if res < 0 {
            return Err(
                "Error: Failed to reopen /dev/tty for interactive input in piped mode".to_string(),
            );
        }
    }
    Ok(())
}

/// Helper function to retrieve flag value and avoid argument hijacking.
fn get_arg(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    if *i + 1 < args.len() {
        let val = &args[*i + 1];
        // Allow negative numbers (like -5) instead of misclassifying them as flags
        let is_negative_num = val.starts_with('-')
            && val.len() > 1
            && val[1..].chars().next().is_some_and(|c| c.is_ascii_digit());
        if val.starts_with('-') && !is_negative_num {
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

/// Parse command-line option arguments from the environment args list.
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
    let mut raw_protocol: Option<ProtocolType> = None;
    let mut explicit_compress: Option<CompressionPolicy> = None;
    let mut disable_kitty_shm = false;
    let mut scale = ScaleMode::Shrink;
    let mut slideshow = None;
    let mut check_magic = false;
    let mut no_thumbnail = false;
    let mut import_path = None;
    let mut export_path = None;
    let mut sync_path = None;
    let mut infobar = InfoBarPosition::Bottom;
    let mut recursive = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--filter" | "-f" => {
                let flag = args[i].clone();
                let val = get_arg(&args, &mut i, &flag)?;
                filter = match val.to_lowercase().as_str() {
                    "nearest" => FilterType::Nearest,
                    "box" => FilterType::Box,
                    "linear" => FilterType::Linear,
                    "mitchell" => FilterType::Mitchell,
                    "lanczos" => FilterType::Lanczos,
                    other => {
                        return Err(format!(
                            "Error: Unknown filter '{}'. Choose from: nearest, box, linear, mitchell, lanczos",
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
                raw_protocol = Some(proto);
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
                    if config.is_active() {
                        slideshow = Some(config);
                    } else {
                        return Err(
                            "Error: -t / --slideshow requires a positive integer argument".into(),
                        );
                    }
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
            "--recursive" | "-R" => {
                recursive = true;
                i += 1;
            }
            "--no-thumbnail" => {
                no_thumbnail = true;
                i += 1;
            }
            "--no-kitty-shm" => {
                disable_kitty_shm = true;
                i += 1;
            }
            "--compress" | "--kitty-compress" => {
                let flag = args[i].clone();
                let val = get_arg(&args, &mut i, &flag)?;
                let mode = match val.to_lowercase().as_str() {
                    "auto" => CompressionPolicy::Auto,
                    "always" => CompressionPolicy::Always,
                    "never" => CompressionPolicy::Never,
                    other => {
                        return Err(format!(
                            "Error: Unknown compress mode '{}'. Choose from: auto, always, never",
                            other
                        ));
                    }
                };
                explicit_compress = Some(mode);
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
            "--version" | "-v" | "-V" => {
                println!("imv-tui {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!("imv-tui: A fast keyboard-driven terminal image viewer");
                println!();
                println!("Usage: imv-tui [path] [options]");
                println!();
                println!("Options:");
                println!(
                    "  -f, --filter <filter>      Initial image scaling filter: nearest, box, linear, mitchell, lanczos"
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
                println!("  -R, --recursive            Scan directories recursively");
                println!(
                    "      --no-thumbnail         Disable low-res EXIF thumbnail placeholder loading"
                );
                println!(
                    "      --compress <mode>      Payload compression mode (Kitty zlib / iTerm2 JPEG / Sixel 128-color): auto, always, never (defaults to auto)"
                );
                println!(
                    "      --no-kitty-shm         Disable Kitty POSIX shared memory (t=s) image transmission"
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
                println!("  -v, --version              Show version information");
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

    let protocol = ProtocolConfig::from_cli(raw_protocol, explicit_compress, disable_kitty_shm)?;

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
        recursive,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ActiveProtocol, Iterm2TransferMode, KittyTransferMode, SixelTransferMode};

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

    #[test]
    fn test_recursive_parsing() {
        let args = vec![
            "imv-tui".to_string(),
            "-R".to_string(),
            "/some/path".to_string(),
        ];
        let opts = parse_cli_args_from(args).unwrap();
        assert!(opts.recursive);
        assert_eq!(opts.initial_path, Some(PathBuf::from("/some/path")));

        let args2 = vec!["imv-tui".to_string(), "--recursive".to_string()];
        let opts2 = parse_cli_args_from(args2).unwrap();
        assert!(opts2.recursive);
        assert_eq!(opts2.initial_path, None);
    }

    #[test]
    fn test_invalid_filter_parameter() {
        let args = vec![
            "imv-tui".to_string(),
            "--filter".to_string(),
            "invalid_filter".to_string(),
        ];
        let err = parse_cli_args_from(args).err().unwrap();
        assert!(err.contains("Unknown filter 'invalid_filter'"));
    }

    #[test]
    fn test_invalid_protocol_parameter() {
        let args = vec![
            "imv-tui".to_string(),
            "-p".to_string(),
            "invalid_proto".to_string(),
        ];
        let err = parse_cli_args_from(args).err().unwrap();
        assert!(err.contains("Unknown protocol 'invalid_proto'"));
    }

    #[test]
    fn test_invalid_scale_parameter() {
        let args = vec![
            "imv-tui".to_string(),
            "--scale".to_string(),
            "invalid_scale".to_string(),
        ];
        let err = parse_cli_args_from(args).err().unwrap();
        assert!(err.contains("Unknown scale mode 'invalid_scale'"));
    }

    #[test]
    fn test_invalid_slideshow_parameter() {
        // slideshow requires positive integer
        let args1 = vec!["imv-tui".to_string(), "-t".to_string(), "abc".to_string()];
        assert!(parse_cli_args_from(args1).is_err());

        let args2 = vec![
            "imv-tui".to_string(),
            "--slideshow".to_string(),
            "-5".to_string(),
        ];
        assert!(parse_cli_args_from(args2).is_err());

        let args3 = vec![
            "imv-tui".to_string(),
            "--slideshow".to_string(),
            "0".to_string(),
        ];
        assert!(parse_cli_args_from(args3).is_err());
    }

    #[test]
    fn test_invalid_infobar_parameter() {
        let args = vec![
            "imv-tui".to_string(),
            "--infobar".to_string(),
            "invalid_pos".to_string(),
        ];
        let err = parse_cli_args_from(args).err().unwrap();
        assert!(err.contains("Unknown infobar position 'invalid_pos'"));
    }

    #[test]
    fn test_option_hijacking() {
        // Attempting to pass another option as a value should be blocked
        let args = vec![
            "imv-tui".to_string(),
            "--filter".to_string(),
            "-s".to_string(),
        ];
        let err = parse_cli_args_from(args).err().unwrap();
        assert!(err.contains("cannot be another option"));
    }

    #[test]
    fn test_multiple_paths() {
        // Only a single path argument is allowed
        let args = vec![
            "imv-tui".to_string(),
            "/path/one".to_string(),
            "/path/two".to_string(),
        ];
        let err = parse_cli_args_from(args).err().unwrap();
        assert!(err.contains("multiple paths were provided"));
    }

    #[test]
    fn test_missing_argument() {
        // Options requiring values at the end of input
        let args = vec!["imv-tui".to_string(), "--filter".to_string()];
        let err = parse_cli_args_from(args).err().unwrap();
        assert!(err.contains("requires an argument"));
    }

    #[test]
    fn test_unknown_option() {
        let args = vec!["imv-tui".to_string(), "--nonexistent-flag".to_string()];
        let err = parse_cli_args_from(args).err().unwrap();
        assert!(err.contains("Unknown option '--nonexistent-flag'"));
    }

    #[test]
    fn test_case_insensitivity_and_aliases() {
        let args = vec![
            "imv-tui".to_string(),
            "--filter".to_string(),
            "LANCZOS".to_string(),
            "--protocol".to_string(),
            "KITTY".to_string(),
            "--scale".to_string(),
            "CROP".to_string(),
        ];
        let opts = parse_cli_args_from(args).unwrap();
        assert_eq!(opts.filter, FilterType::Lanczos);
        assert_eq!(
            opts.protocol,
            ProtocolConfig::Kitty {
                compression: CompressionPolicy::Auto,
                disable_kitty_shm: false,
            }
        );
        assert_eq!(opts.scale, ScaleMode::Crop);

        let args_box = vec![
            "imv-tui".to_string(),
            "--filter".to_string(),
            "BOX".to_string(),
        ];
        let opts_box = parse_cli_args_from(args_box).unwrap();
        assert_eq!(opts_box.filter, FilterType::Box);
    }

    #[test]
    fn test_compress_parsing_and_env_resolution() {
        // 1. Valid parsing for both --compress and backwards-compatible --kitty-compress alias
        for flag in ["--compress", "--kitty-compress"] {
            for (flag_val, expected) in [
                ("auto", CompressionPolicy::Auto),
                ("AUTO", CompressionPolicy::Auto),
                ("Auto", CompressionPolicy::Auto),
                ("always", CompressionPolicy::Always),
                ("ALWAYS", CompressionPolicy::Always),
                ("never", CompressionPolicy::Never),
                ("NEVER", CompressionPolicy::Never),
            ] {
                let args = vec![
                    "imv-tui".to_string(),
                    flag.to_string(),
                    flag_val.to_string(),
                ];
                let opts = parse_cli_args_from(args).unwrap();
                assert_eq!(
                    opts.protocol,
                    ProtocolConfig::Auto {
                        explicit_compress: Some(expected),
                        disable_kitty_shm: false,
                    }
                );
            }
        }

        // 2. Default value
        let default_opts = parse_cli_args_from(["imv-tui".to_string()]).unwrap();
        assert_eq!(
            default_opts.protocol,
            ProtocolConfig::Auto {
                explicit_compress: None,
                disable_kitty_shm: false,
            }
        );

        // 3. Invalid mode rejection
        let invalid_args = vec![
            "imv-tui".to_string(),
            "--compress".to_string(),
            "invalid".to_string(),
        ];
        let err = parse_cli_args_from(invalid_args).err().unwrap();
        assert!(err.contains("Unknown compress mode 'invalid'"));

        // 4. Incompatible protocol combinations fail regardless of argument order
        for (proto, mode) in [("halfblocks", "auto"), ("halfblocks", "always")] {
            let args1 = vec![
                "imv-tui".to_string(),
                "--protocol".to_string(),
                proto.to_string(),
                "--compress".to_string(),
                mode.to_string(),
            ];
            let err1 = parse_cli_args_from(args1).err().unwrap();
            assert_eq!(
                err1,
                "Error: Option --compress can only be used with the kitty, iterm2, or sixel protocol"
            );

            let args2 = vec![
                "imv-tui".to_string(),
                "--kitty-compress".to_string(),
                mode.to_string(),
                "--protocol".to_string(),
                proto.to_string(),
            ];
            let err2 = parse_cli_args_from(args2).err().unwrap();
            assert_eq!(
                err2,
                "Error: Option --compress can only be used with the kitty, iterm2, or sixel protocol"
            );
        }

        // 5. Compatible protocol combinations with Kitty, iTerm2, and Sixel
        let kitty_args = vec![
            "imv-tui".to_string(),
            "--protocol".to_string(),
            "kitty".to_string(),
            "--compress".to_string(),
            "always".to_string(),
        ];
        let kitty_opts = parse_cli_args_from(kitty_args).unwrap();
        assert_eq!(
            kitty_opts.protocol,
            ProtocolConfig::Kitty {
                compression: CompressionPolicy::Always,
                disable_kitty_shm: false,
            }
        );
        assert!(kitty_opts.protocol.should_enable_compression());
        assert!(kitty_opts.protocol.should_enable_kitty_compression());

        let iterm2_args = vec![
            "imv-tui".to_string(),
            "--protocol".to_string(),
            "iterm2".to_string(),
            "--compress".to_string(),
            "always".to_string(),
        ];
        let iterm2_opts = parse_cli_args_from(iterm2_args).unwrap();
        assert_eq!(
            iterm2_opts.protocol,
            ProtocolConfig::Iterm2 {
                compression: CompressionPolicy::Always,
            }
        );
        assert!(iterm2_opts.protocol.should_enable_compression());
        assert!(!iterm2_opts.protocol.should_enable_kitty_compression());

        // Default iTerm2 without --compress uses CompressionPolicy::Auto
        let iterm2_default_args = vec![
            "imv-tui".to_string(),
            "--protocol".to_string(),
            "iterm2".to_string(),
        ];
        let iterm2_default_opts = parse_cli_args_from(iterm2_default_args).unwrap();
        assert_eq!(
            iterm2_default_opts.protocol,
            ProtocolConfig::Iterm2 {
                compression: CompressionPolicy::Auto,
            }
        );

        let sixel_args = vec![
            "imv-tui".to_string(),
            "--protocol".to_string(),
            "sixel".to_string(),
            "--compress".to_string(),
            "always".to_string(),
        ];
        let sixel_opts = parse_cli_args_from(sixel_args).unwrap();
        assert_eq!(
            sixel_opts.protocol,
            ProtocolConfig::Sixel {
                compression: CompressionPolicy::Always,
            }
        );
        assert!(sixel_opts.protocol.should_enable_compression());
        assert!(!sixel_opts.protocol.should_enable_kitty_compression());

        // Default Sixel without --compress uses CompressionPolicy::Auto
        let sixel_default_args = vec![
            "imv-tui".to_string(),
            "--protocol".to_string(),
            "sixel".to_string(),
        ];
        let sixel_default_opts = parse_cli_args_from(sixel_default_args).unwrap();
        assert_eq!(
            sixel_default_opts.protocol,
            ProtocolConfig::Sixel {
                compression: CompressionPolicy::Auto,
            }
        );

        // 6. resolve_active_protocol enforcement
        let auto_explicit = ProtocolConfig::Auto {
            explicit_compress: Some(CompressionPolicy::Always),
            disable_kitty_shm: false,
        };
        let err_hb = auto_explicit
            .resolve_active_protocol(ProtocolType::Halfblocks, true)
            .err()
            .unwrap();
        assert!(
            err_hb
                .contains("detected terminal protocol is Halfblocks (not Kitty, Iterm2, or Sixel)")
        );

        assert_eq!(
            auto_explicit
                .resolve_active_protocol(ProtocolType::Iterm2, true)
                .unwrap(),
            ActiveProtocol::Iterm2(Iterm2TransferMode::Jpeg)
        );
        assert_eq!(
            auto_explicit
                .resolve_active_protocol(ProtocolType::Iterm2, false)
                .unwrap(),
            ActiveProtocol::Iterm2(Iterm2TransferMode::Png)
        );
        assert_eq!(
            auto_explicit
                .resolve_active_protocol(ProtocolType::Sixel, true)
                .unwrap(),
            ActiveProtocol::Sixel(SixelTransferMode::Compressed)
        );
        assert_eq!(
            auto_explicit
                .resolve_active_protocol(ProtocolType::Sixel, false)
                .unwrap(),
            ActiveProtocol::Sixel(SixelTransferMode::Full)
        );

        let auto_default = ProtocolConfig::Auto {
            explicit_compress: None,
            disable_kitty_shm: false,
        };
        assert_eq!(
            auto_default
                .resolve_active_protocol(ProtocolType::Sixel, false)
                .unwrap(),
            ActiveProtocol::Sixel(SixelTransferMode::Full)
        );
        assert_eq!(
            auto_default
                .resolve_active_protocol(ProtocolType::Sixel, true)
                .unwrap(),
            ActiveProtocol::Sixel(SixelTransferMode::Compressed)
        );
        assert_eq!(
            auto_default
                .resolve_active_protocol(ProtocolType::Kitty, true)
                .unwrap(),
            ActiveProtocol::Kitty(KittyTransferMode::Compressed)
        );
        assert_eq!(
            auto_default
                .resolve_active_protocol(ProtocolType::Iterm2, true)
                .unwrap(),
            ActiveProtocol::Iterm2(Iterm2TransferMode::Jpeg)
        );

        // Forced iTerm2 resolution
        let forced_iterm2 = ProtocolConfig::Iterm2 {
            compression: CompressionPolicy::Always,
        };
        assert_eq!(
            forced_iterm2
                .resolve_active_protocol(ProtocolType::Iterm2, true)
                .unwrap(),
            ActiveProtocol::Iterm2(Iterm2TransferMode::Jpeg)
        );
        assert_eq!(
            forced_iterm2
                .resolve_active_protocol(ProtocolType::Iterm2, false)
                .unwrap(),
            ActiveProtocol::Iterm2(Iterm2TransferMode::Png)
        );

        // Forced Sixel resolution
        let forced_sixel = ProtocolConfig::Sixel {
            compression: CompressionPolicy::Always,
        };
        assert_eq!(
            forced_sixel
                .resolve_active_protocol(ProtocolType::Sixel, true)
                .unwrap(),
            ActiveProtocol::Sixel(SixelTransferMode::Compressed)
        );
        assert_eq!(
            forced_sixel
                .resolve_active_protocol(ProtocolType::Sixel, false)
                .unwrap(),
            ActiveProtocol::Sixel(SixelTransferMode::Full)
        );

        // 7. ActiveProtocol helpers
        assert_eq!(
            ActiveProtocol::from_protocol_type(ProtocolType::Kitty, true),
            ActiveProtocol::Kitty(KittyTransferMode::Compressed)
        );
        assert_eq!(
            ActiveProtocol::from_protocol_type(ProtocolType::Iterm2, false),
            ActiveProtocol::Iterm2(Iterm2TransferMode::Png)
        );
        assert_eq!(
            ActiveProtocol::from_protocol_type(ProtocolType::Iterm2, true),
            ActiveProtocol::Iterm2(Iterm2TransferMode::Jpeg)
        );
        assert_eq!(
            ActiveProtocol::from_protocol_type(ProtocolType::Sixel, false),
            ActiveProtocol::Sixel(SixelTransferMode::Full)
        );
        assert_eq!(
            ActiveProtocol::from_protocol_type(ProtocolType::Sixel, true),
            ActiveProtocol::Sixel(SixelTransferMode::Compressed)
        );
        assert_eq!(
            ActiveProtocol::from_protocol_type(ProtocolType::Halfblocks, false),
            ActiveProtocol::Halfblocks
        );

        // 8. should_compress_for_env resolution logic
        assert!(!CompressionPolicy::Auto.should_compress_for_env(false));
        assert!(CompressionPolicy::Auto.should_compress_for_env(true));

        assert!(CompressionPolicy::Always.should_compress_for_env(false));
        assert!(CompressionPolicy::Always.should_compress_for_env(true));

        assert!(!CompressionPolicy::Never.should_compress_for_env(false));
        assert!(!CompressionPolicy::Never.should_compress_for_env(true));
    }

    #[test]
    fn test_no_kitty_shm_flag_parsing_and_validation() {
        // 1. --no-kitty-shm parses cleanly with default (Auto)
        let default_args = vec!["imv-tui".to_string(), "--no-kitty-shm".to_string()];
        let opts_default = parse_cli_args_from(default_args).unwrap();
        assert_eq!(
            opts_default.protocol,
            ProtocolConfig::Auto {
                explicit_compress: None,
                disable_kitty_shm: true,
            }
        );
        assert!(!opts_default.protocol.should_probe_kitty_shm());

        // 2. --no-kitty-shm parses cleanly with -p kitty
        let kitty_args = vec![
            "imv-tui".to_string(),
            "-p".to_string(),
            "kitty".to_string(),
            "--no-kitty-shm".to_string(),
        ];
        let opts_kitty = parse_cli_args_from(kitty_args).unwrap();
        assert_eq!(
            opts_kitty.protocol,
            ProtocolConfig::Kitty {
                compression: CompressionPolicy::Auto,
                disable_kitty_shm: true,
            }
        );
        assert!(!opts_kitty.protocol.should_probe_kitty_shm());

        // 3. Incompatible protocol combinations fail regardless of argument order
        for proto in ["sixel", "iterm2", "halfblocks"] {
            let args1 = vec![
                "imv-tui".to_string(),
                "-p".to_string(),
                proto.to_string(),
                "--no-kitty-shm".to_string(),
            ];
            let err1 = parse_cli_args_from(args1).err().unwrap();
            assert!(err1.contains("--no-kitty-shm"));

            let args2 = vec![
                "imv-tui".to_string(),
                "--no-kitty-shm".to_string(),
                "-p".to_string(),
                proto.to_string(),
            ];
            let err2 = parse_cli_args_from(args2).err().unwrap();
            assert!(err2.contains("--no-kitty-shm"));
        }

        // 4. resolve_active_protocol enforcement on Auto with disable_kitty_shm
        let auto_shm_disabled = ProtocolConfig::Auto {
            explicit_compress: None,
            disable_kitty_shm: true,
        };
        let err = auto_shm_disabled
            .resolve_active_protocol(ProtocolType::Sixel, false)
            .err()
            .unwrap();
        assert!(err.contains("--no-kitty-shm"));

        let err_iterm2 = auto_shm_disabled
            .resolve_active_protocol(ProtocolType::Iterm2, false)
            .err()
            .unwrap();
        assert!(err_iterm2.contains("--no-kitty-shm"));

        // Kitty resolution succeeds and retains Direct transfer mode
        let active_kitty = auto_shm_disabled
            .resolve_active_protocol(ProtocolType::Kitty, false)
            .unwrap();
        assert_eq!(
            active_kitty,
            ActiveProtocol::Kitty(KittyTransferMode::Direct)
        );
    }

    #[test]
    fn test_active_protocol_display_label_and_mode_helpers() {
        let proto = ActiveProtocol::Kitty(KittyTransferMode::SharedMemory);
        assert_eq!(proto.display_label(), "Kitty (SHM)");

        let proto_off = proto.with_kitty_shm(false);
        assert_eq!(proto_off, ActiveProtocol::Kitty(KittyTransferMode::Direct));
        assert_eq!(proto_off.display_label(), "Kitty");

        let proto_on = proto_off.with_kitty_shm(true);
        assert_eq!(
            proto_on,
            ActiveProtocol::Kitty(KittyTransferMode::SharedMemory)
        );

        let proto_comp = ActiveProtocol::Kitty(KittyTransferMode::Compressed);
        assert_eq!(proto_comp.display_label(), "Kitty (Compressed)");

        // with_kitty_shm does not enable shm when compression is active
        let proto_comp_attempt = proto_comp.with_kitty_shm(true);
        assert_eq!(
            proto_comp_attempt,
            ActiveProtocol::Kitty(KittyTransferMode::Compressed)
        );
        assert_eq!(proto_comp_attempt.display_label(), "Kitty (Compressed)");

        // iTerm2 PNG vs JPEG transfer modes
        let iterm2_png = ActiveProtocol::Iterm2(Iterm2TransferMode::Png);
        assert_eq!(iterm2_png.display_label(), "Iterm2");

        let iterm2_jpeg = ActiveProtocol::Iterm2(Iterm2TransferMode::Jpeg);
        assert_eq!(iterm2_jpeg.display_label(), "Iterm2 (JPEG)");

        // Sixel Full vs Compressed transfer modes
        let sixel_full = ActiveProtocol::Sixel(SixelTransferMode::Full);
        assert_eq!(sixel_full.display_label(), "Sixel");

        let sixel_comp = ActiveProtocol::Sixel(SixelTransferMode::Compressed);
        assert_eq!(sixel_comp.display_label(), "Sixel (Compressed)");

        // Halfblocks
        assert_eq!(ActiveProtocol::Halfblocks.display_label(), "Halfblocks");
    }
}
