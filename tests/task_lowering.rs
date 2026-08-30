use spar::runner::TemplatePart;
use spar::{CompileOptions, Compiler};

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
}
task [Build] {
    run { echo second; };
}
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
}
task [Test] {
    default: true;
    run { echo test; };
}
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
}
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
}
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
}
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
}
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
}
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
}
"#;
    let compilation = compile(src);
    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);

    let tasks = compilation.tasks.expect("task set must be lowered");
    let task = tasks.get("deploy").expect("Deploy task must be lowered");
    assert!(task.default);
    assert_eq!(task.commands.len(), 1);
    let parts = &task.commands[0].template.parts;

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
}
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
fn dependencies_and_env_and_cwd_lower_correctly() {
    let src = r#"
task [Prepare] {
    run { echo prepare; };
}
task [Build] {
    dependsOn: [Prepare];
    cwd: "./web";
    env: {
        RUST_LOG: "debug";
    };
    run { cargo build; };
}
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
fn program_with_no_tasks_lowers_to_no_task_set() {
    let src = "export var port: int = 8080;\n";
    let compilation = compile(src);
    assert!(compilation.errors.is_empty(), "{:?}", compilation.errors);
    assert!(compilation.tasks.is_none());
}
