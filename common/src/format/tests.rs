use super::{DisplayCount, DisplaySize, truncate_middle};

#[test]
fn truncate_middle_char_boundary() {
    assert_eq!(
        truncate_middle("굿걸 - 누가 방송국을 털었나 E06.mp4", 44),
        "굿걸 - 누가 방송국을[...]국을 털었나 E06.mp4",
    );
}

#[test]
fn display_count_separates_thousands() {
    let cases = [
        (0, "0"),
        (7, "7"),
        (999, "999"),
        (1_000, "1,000"),
        (12_345, "12,345"),
        (123_456, "123,456"),
        (1_234_567, "1,234,567"),
        (11_341_063, "11,341,063"),
        (u64::MAX, "18,446,744,073,709,551,615"),
    ];
    for (count, expected) in cases {
        assert_eq!(DisplayCount(count).to_string(), expected);
    }
}

#[test]
fn display_count_honours_width() {
    assert_eq!(format!("{:>7}", DisplayCount(1_234)), "  1,234");
}

#[test]
fn display_size_formats_kilobytes() {
    assert_eq!(format!("{}", DisplaySize(2048.0)), "2.0K");
}

mod shell_quote {
    use super::super::{quote_posix, quote_powershell, without_verbatim_prefix};

    #[test]
    fn plain_paths_are_left_alone() {
        for plain in [
            "a",
            "photos/2024/img_01.jpg",
            "a-b+c",
            "/usr/local",
            "café/日本",
        ] {
            assert_eq!(quote_posix(plain.as_bytes()), plain);
            assert_eq!(quote_powershell(plain), plain);
        }
        assert_eq!(quote_posix(b"a,d:e@f"), "a,d:e@f");
    }

    #[test]
    fn posix_quotes_whatever_a_shell_would_read_specially() {
        let cases = [
            ("my files", "'my files'"),
            ("it's", r"'it'\''s'"),
            ("$HOME", "'$HOME'"),
            ("`id`", "'`id`'"),
            ("a;rm -rf x", "'a;rm -rf x'"),
            ("*.txt", "'*.txt'"),
            ("~user", "'~user'"),
            ("=ls", "'=ls'"),
            ("%1", "'%1'"),
            ("wow!", "'wow!'"),
            ("a\\b", r"'a\b'"),
            ("", "''"),
        ];
        for (raw, quoted) in cases {
            assert_eq!(quote_posix(raw.as_bytes()), quoted, "{raw:?}");
        }
    }

    #[test]
    fn posix_escapes_what_must_not_be_pasted_or_displayed_raw() {
        let cases: [(&[u8], &str); 7] = [
            (b"line\nbreak", r"$'line\nbreak'"),
            (b"tab\there", r"$'tab\there'"),
            (b"\x1b[31mred", r"$'\x1b[31mred'"),
            (b"bad\xff name", r"$'bad\xff name'"),
            ("\u{202E}gnp.exe".as_bytes(), r"$'\xe2\x80\xaegnp.exe'"),
            (b"it's\n", r"$'it\'s\n'"),
            ("a\u{2028}b".as_bytes(), r"$'a\xe2\x80\xa8b'"),
        ];
        for (raw, quoted) in cases {
            assert_eq!(quote_posix(raw), quoted, "{raw:?}");
        }
    }

    /// The claim that matters: each shell reads the quoted form back as exactly the original
    /// bytes. Run through every shell present; `sh` is skipped for `$'…'`, which it need not know.
    #[cfg(unix)]
    #[test]
    fn posix_quoting_round_trips_through_real_shells() {
        use ::std::process::Command;

        let names: [&[u8]; 15] = [
            b"plain",
            b"my files",
            b"it's",
            b"$HOME and `id` and $(id)",
            b"a;b|c&d<e>f",
            b"*?[x]{a,b}",
            b"~user",
            b"=ls",
            b"wow!",
            b"back\\slash \\n",
            b"line\nbreak",
            b"\x1b[31mred\x07",
            b"bad\xff\xfe bytes \xc3",
            "\u{202E}gnp.exe\u{200B}".as_bytes(),
            "line\u{2028}sep\u{061C}\u{2060}".as_bytes(),
        ];
        for shell in ["bash", "zsh", "sh"] {
            if Command::new(shell).arg("-c").arg("true").output().is_err() {
                continue;
            }
            for name in names {
                let quoted = quote_posix(name);
                if shell == "sh" && quoted.starts_with("$'") {
                    continue;
                }
                let output = Command::new(shell)
                    .arg("-c")
                    .arg(format!("printf %s {quoted}"))
                    .env("LC_ALL", "en_US.UTF-8")
                    .output()
                    .expect("run shell");
                assert_eq!(
                    output.stdout,
                    name,
                    "{shell} read {quoted} back as {:?}",
                    String::from_utf8_lossy(&output.stdout)
                );
            }
        }
    }

    #[test]
    fn powershell_quotes_its_own_specials() {
        let cases = [
            (r"C:\Users\me\file.txt", r"C:\Users\me\file.txt"),
            (r"C:\My Files", r"'C:\My Files'"),
            ("$env:PATH", "'$env:PATH'"),
            ("it's", "'it''s'"),
            ("it\u{2019}s", "'it\u{2019}\u{2019}s'"),
            ("50%", "'50%'"),
            ("a`b", "'a`b'"),
            ("a;b", "'a;b'"),
            ("a,b", "'a,b'"),
            ("@args", "'@args'"),
            ("", "''"),
        ];
        for (raw, quoted) in cases {
            assert_eq!(quote_powershell(raw), quoted, "{raw:?}");
        }
    }

    #[test]
    fn powershell_escapes_control_and_invisible_characters() {
        assert_eq!(quote_powershell("a\nb"), "\"a`u{A}b\"");
        assert_eq!(
            quote_powershell("\u{202E}$x\"y\u{201C}"),
            "\"`u{202E}`$x`\"y`\u{201C}\""
        );
    }

    #[test]
    fn verbatim_prefixes_are_dropped() {
        assert_eq!(without_verbatim_prefix(r"\\?\C:\Users"), r"C:\Users");
        assert_eq!(
            without_verbatim_prefix(r"\\?\UNC\server\share\x"),
            r"\\server\share\x"
        );
        assert_eq!(without_verbatim_prefix(r"C:\plain"), r"C:\plain");
    }
}

mod relative_to {
    use ::std::path::{Path, PathBuf};

    use super::super::relative_to;

    fn from(base: &str, target: &str) -> Option<PathBuf> {
        relative_to(Path::new(target), Path::new(base))
    }

    #[cfg(unix)]
    #[test]
    fn walks_up_and_down_between_directories() {
        let cases = [
            // cwd /home/user/foo, `duscape ../bar/`, `baz` selected.
            ("/home/user/foo", "/home/user/bar/baz", "../bar/baz"),
            ("/home/user", "/home/user/bar/baz", "bar/baz"),
            ("/home/user/bar/baz/deep", "/home/user/bar/baz", ".."),
            ("/home/user/bar", "/home/user/bar", "."),
            ("/", "/usr/local", "usr/local"),
            ("/usr/local/bin", "/", "../../.."),
            ("/a/b/c", "/x/y", "../../../x/y"),
            // A shared prefix of characters is not a shared directory.
            ("/home/user/foo", "/home/user/foobar", "../foobar"),
        ];
        for (base, target, expected) in cases {
            assert_eq!(
                from(base, target),
                Some(PathBuf::from(expected)),
                "{target} from {base}"
            );
        }
    }

    #[test]
    fn needs_two_absolute_paths() {
        assert_eq!(from("relative/base", "/abs"), None);
        assert_eq!(from("/abs", "relative/target"), None);
    }

    #[cfg(windows)]
    #[test]
    fn stays_on_one_drive() {
        assert_eq!(
            from(r"C:\Users\me\foo", r"C:\Users\me\bar\baz"),
            Some(PathBuf::from(r"..\bar\baz"))
        );
        assert_eq!(from(r"C:\Users", r"D:\data"), None);
    }
}
