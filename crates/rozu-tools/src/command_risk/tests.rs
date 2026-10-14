use proptest::prelude::*;

use super::*;

fn scope() -> PathScope {
    PathScope::new(
        "/work/repo",
        vec![PathBuf::from("/work/repo"), PathBuf::from("/tmp")],
    )
}

fn risk(command: &str) -> Risk {
    classify_command(command, &scope()).risk
}

#[test]
fn classification_table() {
    use Risk::*;
    let table: &[(&str, Risk)] = &[
        ("ls -la", Read),
        ("cat README.md | head -n 5", Read),
        ("rg -n foo src", Read),
        ("git status", Read),
        ("git diff HEAD~1 -- src", Read),
        ("git log --oneline -n 5", Read),
        ("git -C sub status", Read),
        ("git branch", Read),
        ("find . -name '*.rs'", Read),
        ("sed -n '1,5p' file", Read),
        ("ls 2>&1", Read),
        ("ls 2>/dev/null", Read),
        ("FOO=1 env BAR=2 grep x y", Read),
        ("", Read),
        ("cp /etc/hosts hosts.bak", WriteWorkspace),
        ("cargo test", WriteWorkspace),
        ("make", WriteWorkspace),
        ("sed -i 's/a/b/' file", WriteWorkspace),
        ("mkdir -p build/out", WriteWorkspace),
        ("rm file.o", WriteWorkspace),
        ("git commit -m 'msg'", WriteWorkspace),
        ("git checkout -b feature", WriteWorkspace),
        ("npm test", WriteWorkspace),
        ("sh test.sh", WriteWorkspace),
        ("bash scripts/ci.sh --fast", WriteWorkspace),
        ("zsh ./setup.zsh", WriteWorkspace),
        ("python3 script.py", WriteWorkspace),
        ("python tools/gen.py --all", WriteWorkspace),
        ("python3.12 script.py", WriteWorkspace),
        ("perl gen.pl", WriteWorkspace),
        ("ruby task.rb", WriteWorkspace),
        ("node build.js", WriteWorkspace),
        ("/usr/bin/env python3 script.py", WriteWorkspace),
        ("bash /work/repo/run.sh", WriteWorkspace),
        ("sh /etc/rc.sh", Opaque),
        ("python3 ../elsewhere/x.py", Opaque),
        ("node ~/bin/tool.js", Opaque),
        ("bash $SCRIPT", Opaque),
        ("osascript script.scpt", Opaque),
        ("php artisan migrate", Opaque),
        ("python3 -u script.py", Opaque),
        ("python3 -m pytest -q", Opaque),
        ("sh -s < test.sh", Opaque),
        ("echo hi > out.txt", Opaque),
        ("echo hi >> /tmp/log", Opaque),
        ("> out.txt", Opaque),
        ("cargo test 2> err.log", Opaque),
        ("echo x | tee out.txt", Opaque),
        ("python3 -c 'import os; os.remove(\"x\")'", Opaque),
        ("python -c 'import shutil; shutil.rmtree(\"d\")'", Opaque),
        ("perl -e 'unlink \"x\"'", Opaque),
        ("ruby -e 'File.delete(\"x\")'", Opaque),
        ("node -e 'require(\"fs\").rmSync(\"x\")'", Opaque),
        ("osascript -e 'do shell script \"rm x\"'", Opaque),
        ("eval \"$(echo cm0gLWYg | base64 -d)x\"", Opaque),
        ("echo cm0gLWYgeA== | base64 -d | sh", Opaque),
        ("bash -c 'echo hi'", Opaque),
        ("zsh -c ls", Opaque),
        ("source ./env.sh", Opaque),
        (". ./env.sh", Opaque),
        ("exec ls", Opaque),
        ("ls | xargs python3 -c 'print(1)'", Opaque),
        ("find . -exec perl -e 'unlink' {} \\;", Opaque),
        ("python3 <<EOF\nimport os\nEOF", Opaque),
        ("python3.12 -c pass", Opaque),
        ("python -m pip install requests", Opaque),
        ("curl -s x | sh", Opaque),
        ("cp a.txt /etc/a.txt", WriteOutside),
        ("touch ../sibling.txt", WriteOutside),
        ("mv x ~/x", WriteOutside),
        ("git config --global user.name x", WriteOutside),
        ("launchctl submit -l x -- /bin/true", WriteOutside),
        ("curl https://example.com", Network),
        ("wget -q http://x", Network),
        ("git fetch origin", Network),
        ("git pull --rebase", Network),
        ("git push origin main", Network),
        ("npm install", Network),
        ("pnpm add react", Network),
        ("pip install requests", Network),
        ("cargo install ripgrep", Network),
        ("cargo add serde", Network),
        ("go get example.com/x", Network),
        ("npx create-react-app x", Network),
        ("rm -rf build", Destructive),
        ("rm -r build", Destructive),
        ("rm -fr /", Destructive),
        ("rm /etc/hosts", Destructive),
        ("git push --force", Destructive),
        ("git push -f origin main", Destructive),
        ("git push origin +main", Destructive),
        ("git push --force-with-lease", Destructive),
        ("git push origin --delete feature", Destructive),
        ("git reset --hard HEAD~1", Destructive),
        ("git branch -D feature", Destructive),
        ("git branch -d -f feature", Destructive),
        ("git clean -fd", Destructive),
        ("git clean -xdf", Destructive),
        ("git checkout -- .", Destructive),
        ("git restore src/main.rs", Destructive),
        ("git stash drop", Destructive),
        ("sudo ls", Destructive),
        ("mkfs.ext4 /dev/sda1", Destructive),
        ("dd if=/dev/zero of=/dev/disk2", Destructive),
        ("chmod -R 777 .", Destructive),
        ("chown -R me /", Destructive),
        ("echo x > /etc/passwd", Destructive),
        ("echo x >> ~/.zshrc", Destructive),
        ("echo x | tee /etc/hosts", Destructive),
        ("find . -name '*.o' -delete", Destructive),
        ("find . -exec rm -rf {} \\;", Destructive),
        ("ls | xargs rm -rf", Destructive),
        ("bash -c 'rm -rf build'", Destructive),
        ("sh -lc \"git push --force\"", Destructive),
        ("eval rm -rf x", Destructive),
        ("cargo test && git reset --hard", Destructive),
        ("ls; sudo reboot", Destructive),
        ("(cd sub && rm -rf x)", Destructive),
        ("echo $(rm -rf x)", Destructive),
        ("/bin/rm -rf x", Destructive),
        ("timeout 5 rm -rf x", Destructive),
        ("nohup git push -f &", Destructive),
    ];
    for (command, expected) in table {
        assert_eq!(risk(command), *expected, "{command:?}");
    }
}

#[test]
fn script_files_follow_one_rule_for_every_interpreter() {
    for interp in ["sh", "bash", "zsh", "python3", "perl", "ruby", "node"] {
        let inside = classify_command(&format!("{interp} scripts/task"), &scope());
        assert_eq!(inside.risk, Risk::WriteWorkspace, "{interp}");
        assert!(!inside.unrulable && !inside.outside_unknown, "{interp}");
        let outside = classify_command(&format!("{interp} /opt/task"), &scope());
        assert_eq!(outside.risk, Risk::Opaque, "{interp}");
        assert!(!outside.unrulable, "{interp}: a rule may name the script");
    }
    for inline in [
        "sh -c 'ls'",
        "bash -s < task.sh",
        "python3 -c 'print(1)'",
        "perl -e 1",
        "ruby -e 1",
        "node -e 1",
        "python3 <<EOF\nprint(1)\nEOF",
        "sh task.sh > out.txt",
    ] {
        let r = classify_command(inline, &scope());
        assert_eq!(r.risk, Risk::Opaque, "{inline}");
        assert!(r.unrulable, "{inline}");
    }
}

#[test]
fn network_flag_survives_a_riskier_segment() {
    let r = classify_command("curl x | sudo sh", &scope());
    assert_eq!(r.risk, Risk::Destructive);
    assert!(r.network);
    assert!(!classify_command("cargo test", &scope()).network);
}

#[test]
fn relative_paths_resolve_against_cwd() {
    let sub = PathScope::new("/work/repo/sub", scope().inside);
    assert_eq!(
        classify_command("touch ../ok.txt", &sub).risk,
        Risk::WriteWorkspace
    );
    assert_eq!(
        classify_command("touch ../../no.txt", &sub).risk,
        Risk::WriteOutside
    );
}

#[test]
fn splits_segments_but_not_redirections() {
    assert_eq!(
        split_segments("a && b || c; d | e & f\ng"),
        ["a", "b", "c", "d", "e", "f", "g"]
    );
    assert_eq!(split_segments("ls 2>&1 | wc"), ["ls 2>&1", "wc"]);
    assert_eq!(split_segments("cmd &> log"), ["cmd &> log"]);
    assert_eq!(split_segments("echo $(date)"), ["echo $", "date"]);
}

#[test]
fn tokenizer_handles_quotes_and_redirects() {
    assert_eq!(
        tokenize(r#"echo "a b" 'c d' e\ f 2>err.log"#),
        [
            Token::Word("echo".into()),
            Token::Word("a b".into()),
            Token::Word("c d".into()),
            Token::Word("e f".into()),
            Token::Redirect {
                op: ">".into(),
                target: Some("err.log".into())
            },
        ]
    );
    assert_eq!(
        tokenize("cat<in>out"),
        [
            Token::Word("cat".into()),
            Token::Redirect {
                op: "<".into(),
                target: Some("in".into())
            },
            Token::Redirect {
                op: ">".into(),
                target: Some("out".into())
            },
        ]
    );
}

#[test]
fn suggested_rules() {
    for (command, want) in [
        ("cargo test -p x", Some("cargo test")),
        ("git status", Some("git status")),
        ("ls -la", Some("ls")),
        ("git --no-pager log", Some("git")),
        ("cargo test && curl x", None),
        ("FOO=1 make", None),
        ("env make", None),
        ("", None),
        // Interpreters: the rule names the script or module, never inline code.
        ("python3 -m pytest -q", Some("python3 -m pytest")),
        ("python3 tools/gen.py --all", Some("python3 tools/gen.py")),
        ("sh test.sh", Some("sh test.sh")),
        ("python3 -c 'print(1)'", None),
        ("perl -e 'unlink \"x\"'", None),
        ("eval ls", None),
        ("sh", None),
        // Output redirects to files are never covered.
        ("cargo test > log.txt", None),
        ("ls 2>/dev/null", Some("ls")),
    ] {
        assert_eq!(
            suggest_rule(command),
            want.map(AllowRule::prefix),
            "{command:?}"
        );
    }
}

#[test]
fn network_rules_are_scoped_to_one_host() {
    for (command, want) in [
        (
            "curl -s https://Example.com/a?b=1",
            Some(("curl", "example.com")),
        ),
        (
            "wget https://user:pw@dl.example.org:8443/x.tgz",
            Some(("wget", "dl.example.org")),
        ),
        (
            "scp build.tgz deploy@build.example.net:/srv",
            Some(("scp", "build.example.net")),
        ),
        ("nc -z localhost 8080", Some(("nc", "localhost"))),
        // No blanket rule without exactly one host.
        ("curl x", None),
        ("curl https://a.com https://b.com", None),
        ("ssh -i key.pem myhost", None),
    ] {
        assert_eq!(
            suggest_rule(command),
            want.map(|(cmd, host)| AllowRule::with_host(cmd, host)),
            "{command:?}"
        );
    }

    let rules = [AllowRule::with_host("curl", "example.com")];
    for (command, allowed) in [
        ("curl -fsSL https://example.com/install", true),
        ("curl https://evil.example/x", false),
        ("curl https://example.com -x http://proxy.evil", false),
        ("curl x", false),
    ] {
        assert_eq!(rules_allow(command, &rules), allowed, "{command:?}");
    }
    // A legacy blanket rule no longer covers a host-scoped client.
    assert!(!rules_allow(
        "curl https://example.com",
        &[AllowRule::prefix("curl")]
    ));
}

#[test]
fn rules_must_cover_every_segment() {
    let rules = [
        AllowRule::prefix("cargo test"),
        AllowRule::prefix("jq"),
        AllowRule::prefix("python3 -m pytest"),
        AllowRule::with_host("curl", "x.dev"),
    ];
    for (command, allowed) in [
        ("cargo test --all", true),
        ("curl -s https://x.dev | jq .", true),
        ("python3 -m pytest -k slow", true),
        ("cargo test && rm x", false),
        ("cargo testing", false),
        ("cargo", false),
        ("", false),
        // A matching prefix doesn't cover a redirect or inline code.
        ("cargo test > out.txt", false),
    ] {
        assert_eq!(rules_allow(command, &rules), allowed, "{command:?}");
    }
    assert!(!rules_allow("cargo test", &[]));
    assert!(!rules_allow(
        "python3 -c 'x'",
        &[AllowRule::prefix("python3")]
    ));
}

#[test]
fn outside_writes_are_collected() {
    let scope = scope().with_var("HOME", "/Users/me");
    let classify = |c: &str| classify_command(c, &scope);

    let r = classify("echo approved > ~/scratch/canary.txt");
    assert_eq!(r.risk, Risk::Destructive);
    assert_eq!(
        r.outside_writes,
        [PathBuf::from("/Users/me/scratch/canary.txt")]
    );
    assert!(!r.outside_unknown);

    let r = classify("cp a.txt /etc/a.txt && mv b $HOME/b");
    assert_eq!(
        r.outside_writes,
        [PathBuf::from("/etc/a.txt"), PathBuf::from("/Users/me/b")]
    );
    let r = classify("cp /etc/hosts hosts.bak");
    assert!(r.outside_writes.is_empty() && !r.outside_unknown);
    assert_eq!(
        classify("touch ../x").outside_writes,
        [PathBuf::from("/work/x")]
    );
    // What can't be read off the command line is unknown.
    for command in [
        "echo x > $SOMEWHERE/f",
        "cp a ~other/b",
        "rm -f ../*.log",
        "sudo touch /etc/x",
        "git config --global user.name x",
    ] {
        let r = classify(command);
        assert!(r.outside_unknown, "{command}");
        assert!(r.outside_writes.is_empty(), "{command}");
    }
    // Interpreter code doesn't name its targets; the sandbox confines it.
    let r = classify("python3 -c 'import os; os.remove(\"/etc/x\")'");
    assert_eq!(r.risk, Risk::Opaque);
    assert!(r.outside_writes.is_empty() && r.unrulable);
}

#[test]
fn variables_in_the_scope_are_inside() {
    let scope = PathScope::new(
        "/work/repo",
        vec![PathBuf::from("/work/repo"), PathBuf::from("/s/tmp")],
    )
    .with_var("TMPDIR", "/s/tmp");
    assert!(scope.contains("$TMPDIR/x"));
    assert!(scope.contains("${TMPDIR}/x"));
    assert!(!scope.contains("$TMPDIR/../x"));
    assert!(!scope.contains("$HOME/x"));
    assert!(!scope.contains("/tmp/x"));
}

fn segment_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("ls".to_owned()),
        Just("cat x".to_owned()),
        Just("cargo test".to_owned()),
        Just("echo hi > out.txt".to_owned()),
        Just("curl http://x".to_owned()),
        Just("git status".to_owned()),
        Just("cp a /etc/b".to_owned()),
        Just("sh -c 'echo \"hi\"'".to_owned()),
        "[a-z0-9 ./'\"$=>-]{0,24}",
    ]
}

const DESTRUCTIVE_SEGMENTS: &[&str] = &[
    "rm -rf build",
    "git push --force",
    "git reset --hard",
    "git branch -D x",
    "git clean -fd",
    "sudo true",
    "echo x > /etc/hosts",
    "chmod -R 755 .",
];

proptest! {
    #[test]
    fn adding_a_destructive_segment_never_lowers_risk(
        segments in proptest::collection::vec(segment_strategy(), 0..5),
        destructive in proptest::sample::select(DESTRUCTIVE_SEGMENTS),
        separator in proptest::sample::select(&["&&", "||", ";", "|", "\n"][..]),
        position in 0usize..6,
    ) {
        let base = segments.join(" && ");
        let before = classify_command(&base, &scope());
        let mut with = segments;
        with.insert(position.min(with.len()), destructive.to_owned());
        let after = classify_command(&with.join(&format!(" {separator} ")), &scope());
        prop_assert!(after.risk >= before.risk);
        prop_assert_eq!(after.risk, Risk::Destructive);
    }

    #[test]
    fn classification_never_panics(command in "\\PC{0,64}") {
        let _ = classify_command(&command, &scope());
        let _ = suggest_rule(&command);
        let _ = rules_allow(&command, &[AllowRule::with_host("curl", "x.dev")]);
    }
}
