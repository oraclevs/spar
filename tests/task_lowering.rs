use spar::runner::TemplatePart;
use spar::{CompileOptions, Compiler};
use std::fs;
use tempfile::tempdir;

fn compile(src: &str) -> spar::Compilation {
    Compiler::new(CompileOptions::default()).compile(src)
}

fn errors_contain(compilation: &spar::Compilation, needle: &str) -> bool {
    compilation
        .errors
        .iter()
        .any(|e| format!("{e}").contains(needle) || format!("{e:?}").contains(needle))
}

#[test]
fn duplicate_task_names_produce_a_diagnostic() {
    let src = r#"
task [Build] {
    run { echo first; };
};
task [Build] {
    run { echo second; };
};
"#;
    let compilation = compile(src);
    assert!(!compilation.errors.is_empty());
    assert!(
        errors_contain(&compilation, "already defined"),
        "{:?}",
        compilation.errors
    );
}

#[test]
fn duplicate_default_tasks_produce_a_diagnostic() {
    let src = r#"
task [Build] {
    default: true;
    run { echo build; };
};
task [Test] {
    default: true;
    run { echo test; };
};
"#;
    let compilation = compile(src);
    assert!(!compilation.errors.is_empty());
    assert!(
        errors_contain(&compilation, "multiple default tasks"),
        "{:?}",
        compilation.errors
    );
}

#[test]
fn unknown_dependency_produces_a_diagnostic_before_running_anything() {
    let src = r#"
task [Test] {
    dependsOn: [DoesNotExist];
    run { echo test; };
};
"#;
    let compilation = compile(src);
    assert!(!compilation.errors.is_empty());
    assert!(
        errors_contain(&compilation, "unknown task"),
        "{:?}",
        compilation.errors
    );
}

#[test]
fn bad_metadata_type_produces_a_diagnostic() {
    let src = r#"
task [Build] {
    default: "yes";
    run { echo build; };
};
"#;
    let compilation = compile(src);
    assert!(!compilation.errors.is_empty());
    assert!(
        errors_contain(&compilation, "default") && errors_contain(&compilation, "bool"),
        "{:?}",
        compilation.errors
    );
}

#[test]
fn non_scalar_parameter_produces_a_diagnostic() {
    let src = r#"
task [Build](names: [str]) {
    run { echo build; };
};
"#;
    let compilation = compile(src);
    assert!(!compilation.errors.is_empty());
    assert!(
        errors_contain(&compilation, "scalar") || errors_contain(&compilation, "str"),
        "{:?}",
        compilation.errors
    );
}

#[test]
fn task_local_unknown_name_in_run_body_produces_a_diagnostic() {
    let src = r#"
task [Build] {
    run { echo ${nope}; };
};
"#;
    let compilation = compile(src);
    assert!(!compilation.errors.is_empty());
}

#[test]
fn parameter_combined_with_other_values_in_one_interpolation_is_rejected() {
    let src = r#"
export var suffix: str = "prod";

task [Deploy](environment: str) {
    run { ./deploy.sh ${environment + suffix}; };
};
"#;
    let compilation = compile(src);
    assert!(!compilation.errors.is_empty());
    assert!(
        errors_contain(&compilation, "cannot combine a task parameter"),
        "{:?}",
        compilation.errors
    );
}

/// Critical architecture proof (Test 2 from the design): a normal Spar
/// configuration value AND a task argument both reach the same command
/// template — proving `task_lowering` is genuinely wired into the Spar
/// evaluator, not an unrelated embedded implementation.
#[test]
fn spar_value_and_task_argument_both_reach_the_command_template() {
    let src = r#"
export var port: int = 8080;

task [Deploy](environment: str) {
    default: true;

    run {
        cargo run -- --port ${port} --env ${environment};
    };
};
"#;
    let compilation = compile(src);
    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);

    let tasks = compilation.tasks.expect("task set must be lowered");
    let task = tasks.get("deploy").expect("Deploy task must be lowered");
    assert!(task.default);
    assert_eq!(task.commands.len(), 1);
    let parts = &task.commands[0].template().parts;

    let has_evaluated_port = parts
        .iter()
        .any(|p| matches!(p, TemplatePart::Literal(s) if s.contains("8080")));
    assert!(
        has_evaluated_port,
        "the ordinary Spar value `port` must be pre-evaluated into the template: {parts:?}"
    );

    let has_neutral_param_slot = parts
        .iter()
        .any(|p| matches!(p, TemplatePart::Parameter(name) if name == "environment"));
    assert!(
        has_neutral_param_slot,
        "the task parameter `environment` must stay a neutral slot until CLI binding: {parts:?}"
    );
}

#[test]
fn task_with_no_parameters_and_no_metadata_lowers_cleanly() {
    let src = r#"
task [Build] {
    run {
        cargo build;
    };
};
"#;
    let compilation = compile(src);
    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    let tasks = compilation.tasks.expect("task set must be lowered");
    let task = tasks.get("build").unwrap();
    assert!(!task.default);
    assert!(task.dependencies.is_empty());
    assert!(task.parameters.is_empty());
}

#[test]
fn hash_escape_lowers_to_literal_shell_parameter_expansion() {
    let src = r#"
task [Build] {
    run {
        echo #{HOME:-x};
    };
};
"#;
    let compilation = compile(src);
    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    let tasks = compilation.tasks.expect("task set must be lowered");
    let task = tasks.get("build").expect("Build task must be lowered");

    assert_eq!(task.commands[0].render_unbound(), "echo ${HOME:-x}");
}

#[test]
fn dependencies_and_env_and_cwd_lower_correctly() {
    let src = r#"
task [Prepare] {
    run { echo prepare; };
};
task [Build] {
    dependsOn: [Prepare];
    cwd: "./web";
    env: {
        RUST_LOG: "debug";
    };
    run { cargo build; };
};
"#;
    let compilation = compile(src);
    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    let tasks = compilation.tasks.expect("task set must be lowered");
    let build = tasks.get("build").unwrap();
    assert_eq!(build.dependencies, vec!["Prepare".to_string()]);
    assert_eq!(build.cwd.as_deref().unwrap().to_str().unwrap(), "./web");
    assert_eq!(build.environment.get("RUST_LOG").unwrap(), "debug");
}

#[test]
fn bare_load_env_loads_dotenv_from_the_base_directory() {
    let directory = tempdir().unwrap();
    fs::write(
        directory.path().join(".env"),
        "SPAR_TEST_BARE_LOAD_ENV=bare\n",
    )
    .unwrap();
    let compilation = Compiler::new(CompileOptions {
        base_dir: directory.path().to_path_buf(),
        ..CompileOptions::default()
    })
    .compile("@LoadEnv\ntask [Build] { run { echo build; }; };");

    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    let tasks = compilation.tasks.expect("task set must be lowered");
    assert_eq!(
        tasks
            .get("build")
            .unwrap()
            .environment
            .get("SPAR_TEST_BARE_LOAD_ENV")
            .map(String::as_str),
        Some("bare")
    );
}

#[test]
fn load_env_custom_path_is_resolved_from_the_base_directory() {
    let directory = tempdir().unwrap();
    fs::write(
        directory.path().join(".env.production"),
        "SPAR_TEST_CUSTOM_LOAD_ENV=production\n",
    )
    .unwrap();
    let compilation = Compiler::new(CompileOptions {
        base_dir: directory.path().to_path_buf(),
        ..CompileOptions::default()
    })
    .compile("@LoadEnv(\".env.production\")\ntask [Build] { run { echo build; }; };");

    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    let tasks = compilation.tasks.expect("task set must be lowered");
    assert_eq!(
        tasks
            .get("build")
            .unwrap()
            .environment
            .get("SPAR_TEST_CUSTOM_LOAD_ENV")
            .map(String::as_str),
        Some("production")
    );
}

#[test]
fn program_with_no_tasks_lowers_to_no_task_set() {
    let src = "export var port: int = 8080;\n";
    let compilation = compile(src);
    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    assert!(compilation.tasks.is_none());
}

#[test]
fn lowers_v2_metadata_defaults_and_shebang_commands() {
    let compilation = compile(
        r#"
task [Deploy](environment: str = "staging", *extra: str) {
    private: true;
    group: "release";
    confirm: "Really deploy?";
    shell: ["bash", "-c"];
    run {
        #!/usr/bin/env bash
        echo ${environment} ${extra}
    };
};
"#,
    );
    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    let tasks = compilation.tasks.unwrap();
    let task = tasks.get("deploy").unwrap();
    assert!(task.private);
    assert_eq!(task.group.as_deref(), Some("release"));
    assert_eq!(task.confirm.as_deref(), Some("Really deploy?"));
    assert_eq!(
        task.shell.as_deref(),
        Some(["bash".to_string(), "-c".to_string()].as_slice())
    );
    assert_eq!(task.parameters[0].default.as_deref(), Some("staging"));
    assert!(task.parameters[1].variadic);
    assert!(matches!(
        task.commands[0],
        spar::runner::TaskCommand::Script(_)
    ));
}

#[test]
fn run_block_matching_current_os_is_selected_over_default() {
    let os = std::env::consts::OS;
    let src = format!(
        r#"
task [T] {{
    run {{
        echo default;
    }};
    run {os} {{
        echo current-os;
    }};
}};
"#
    );
    let compilation = compile(&src);
    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    let tasks = compilation.tasks.expect("task set must be lowered");
    let task = tasks.get("t").expect("T task must be lowered");
    assert_eq!(task.commands.len(), 1);
    assert_eq!(task.commands[0].render_unbound(), "echo current-os");
}

#[test]
fn default_run_block_is_selected_when_no_os_specific_block_matches() {
    let other_os = ["windows", "linux", "macos"]
        .into_iter()
        .find(|candidate| *candidate != std::env::consts::OS)
        .unwrap();
    let src = format!(
        r#"
task [T] {{
    run {{
        echo default;
    }};
    run {other_os} {{
        echo other;
    }};
}};
"#
    );
    let compilation = compile(&src);
    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    let tasks = compilation.tasks.expect("task set must be lowered");
    let task = tasks.get("t").expect("T task must be lowered");
    assert_eq!(task.commands.len(), 1);
    assert_eq!(task.commands[0].render_unbound(), "echo default");
}

#[test]
fn missing_run_block_for_current_os_with_no_default_is_a_clear_lowering_error() {
    let other_oses: Vec<&str> = ["windows", "linux", "macos"]
        .into_iter()
        .filter(|candidate| *candidate != std::env::consts::OS)
        .collect();
    let src = format!(
        r#"
task [T] {{
    run {} {{
        echo one;
    }};
    run {} {{
        echo two;
    }};
}};
"#,
        other_oses[0], other_oses[1]
    );
    let compilation = compile(&src);
    assert!(!compilation.errors.is_empty());
    assert!(
        errors_contain(&compilation, std::env::consts::OS)
            && errors_contain(&compilation, "default"),
        "{:?}",
        compilation.errors
    );
}
