use std::io::Write;
use std::process::{Command, Stdio};

fn spar_exec_with_input(source: &str, input: &str) -> std::process::Output {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("script.spar");
    std::fs::write(&path, source).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_spar"))
        .args(["exec", path.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    // `directory` must outlive `child` (the script path must stay valid until
    // the subprocess reads it) but can drop now that it has exited.
    drop(directory);
    output
}

#[test]
fn read_str_binds_value_visible_to_a_later_statement() {
    let output = spar_exec_with_input(
        r#"
        function main() -> shell {
            return shell {
                read str name;
                echo "hi $name";
            };
        };
        "#,
        "Ada\n",
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"hi Ada\n");
}

#[test]
fn read_int_binds_value_visible_to_a_later_statement() {
    let output = spar_exec_with_input(
        r#"
        function main() -> shell {
            return shell {
                read int age;
                echo "age=$age";
            };
        };
        "#,
        "7\n",
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"age=7\n");
}

#[test]
fn read_float_binds_value_visible_to_a_later_statement() {
    let output = spar_exec_with_input(
        r#"
        function main() -> shell {
            return shell {
                read float ratio;
                echo "ratio=$ratio";
            };
        };
        "#,
        "3.5\n",
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"ratio=3.5\n");
}

#[test]
fn read_bool_binds_value_visible_to_a_later_statement() {
    let output = spar_exec_with_input(
        r#"
        function main() -> shell {
            return shell {
                read bool ok;
                echo "ok=$ok";
            };
        };
        "#,
        "true\n",
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"ok=true\n");
}

#[test]
fn read_wrong_arity_is_a_runtime_error() {
    let output = spar_exec_with_input(
        r#"
        function main() -> shell {
            return shell { read name; };
        };
        "#,
        "",
    );
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("requires a type and a variable name"),
        "{stderr}"
    );
}

#[test]
fn read_unknown_type_token_is_a_runtime_error() {
    let output = spar_exec_with_input(
        r#"
        function main() -> shell {
            return shell { read frobnicate name; };
        };
        "#,
        "",
    );
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown type 'frobnicate'"), "{stderr}");
}

#[test]
fn read_int_rejects_non_numeric_input() {
    let output = spar_exec_with_input(
        r#"
        function main() -> shell {
            return shell { read int n; };
        };
        "#,
        "abc\n",
    );
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("'abc' is not a valid int"), "{stderr}");
}

#[test]
fn read_float_rejects_non_numeric_input() {
    let output = spar_exec_with_input(
        r#"
        function main() -> shell {
            return shell { read float n; };
        };
        "#,
        "abc\n",
    );
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("'abc' is not a valid float"), "{stderr}");
}

#[test]
fn read_bool_rejects_non_bool_input() {
    let output = spar_exec_with_input(
        r#"
        function main() -> shell {
            return shell { read bool n; };
        };
        "#,
        "yes\n",
    );
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("'yes' is not a valid bool"), "{stderr}");
}
