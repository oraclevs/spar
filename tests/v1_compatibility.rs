use std::fs;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use spar::ast::SparType;
use spar::{ConfigValue, Engine, HostFunction, HostRegistry};

fn spar_in(args: &[&str], directory: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_spar"))
        .args(args)
        .current_dir(directory)
        .output()
        .unwrap()
}

#[test]
fn check_emit_and_execute_remain_separate_modes() {
    let calls = Arc::new(Mutex::new(0));
    let calls_from_host = calls.clone();
    let mut hosts = HostRegistry::new();
    hosts
        .register(HostFunction::new(
            "probe",
            "touch",
            vec![],
            SparType::Void,
            move |_| {
                *calls_from_host.lock().unwrap() += 1;
                Ok(ConfigValue::Int(0))
            },
        ))
        .unwrap();

    let engine = Engine::default().with_hosts(hosts);
    let source = r#"
        export var answer: int = 42;
        function main() -> void { probe::touch(); };
    "#;

    engine.check_source(source).unwrap();
    assert_eq!(*calls.lock().unwrap(), 0, "check evaluated source");

    let emitted = engine.emit_source(source).into_result().unwrap();
    assert_eq!(
        emitted.result.unwrap().globals["answer"],
        ConfigValue::Int(42)
    );
    assert_eq!(*calls.lock().unwrap(), 0, "emit invoked main");

    let outcome = engine.execute_source(source).unwrap();
    assert_eq!(outcome.exit_status, 0);
    assert_eq!(
        *calls.lock().unwrap(),
        1,
        "execute did not invoke main once"
    );
}

#[test]
fn execute_never_invokes_an_imported_modules_main() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("library.spar"),
        r#"
            export var value: int = 6;
            function main() -> int { return 99; };
        "#,
    )
    .unwrap();
    fs::write(
        directory.path().join("app.spar"),
        r#"
            import "library.spar" as library;
            var importedValue: int = library.value;
            function main() -> int { return importedValue; };
        "#,
    )
    .unwrap();

    let outcome = Engine::default()
        .execute_path(&directory.path().join("app.spar"))
        .unwrap();

    assert_eq!(outcome.exit_status, 6);
}

#[test]
fn shell_main_maps_the_plan_status() {
    let success = Engine::default()
        .execute_source("function main() -> shell { return shell { true; }; };")
        .unwrap();
    let failure = Engine::default()
        .execute_source("function main() -> shell { return shell { false; }; };")
        .unwrap();

    assert_eq!(success.exit_status, 0);
    assert_ne!(failure.exit_status, 0);
}

#[test]
fn emit_keeps_public_configuration_and_hides_internal_values() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("config.spar"),
        r#"
            var internal: int = 1;
            export var visible: int = 2;
            [Public] { name: str = "spar"; };
            private [Private] { token: str = "hidden"; };
        "#,
    )
    .unwrap();

    let output = spar_in(&["emit", "config.spar"], directory.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(json["visible"], 2);
    assert_eq!(json["Public"]["name"], "spar");
    assert!(json.get("internal").is_none());
    assert!(json.get("Private").is_none());
}

#[test]
fn task_named_exec_and_script_exec_remain_distinct() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("SparMake.spar"),
        concat!(
            "task Exec {\n",
            "    run linux { touch task-ran; };\n",
            "    run macos { touch task-ran; };\n",
            "};\n",
        ),
    )
    .unwrap();
    fs::write(
        directory.path().join("app.spar"),
        "function main() -> int { return 7; };",
    )
    .unwrap();

    let task = spar_in(&["run", "exec"], directory.path());
    assert!(
        task.status.success(),
        "{}",
        String::from_utf8_lossy(&task.stderr)
    );
    assert!(directory.path().join("task-ran").exists());

    let script = spar_in(&["exec", "app.spar"], directory.path());
    assert_eq!(script.status.code(), Some(7), "{script:?}");
}

#[test]
fn unix_shell_pipe_keeps_existing_process_semantics() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            function main() -> shell {
                return shell { printf spar | cat; };
            };
            "#,
        )
        .expect("ordinary shell pipe must remain a Unix process pipe");
    assert_eq!(outcome.exit_status, 0);
}
