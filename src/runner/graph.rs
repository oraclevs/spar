use std::collections::{BTreeMap, BTreeSet};

use super::{BoundValue, RunnerError, ScalarKind, Task, TaskInvocation, TaskSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPlan {
    pub tasks: Vec<BoundTask>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundTask {
    pub task: Task,
    pub parameter_values: BTreeMap<String, BoundValue>,
}

pub(super) fn plan(
    task_set: &TaskSet,
    invocation: &TaskInvocation,
) -> Result<ExecutionPlan, RunnerError> {
    let requested_task = task_set.get(&invocation.task)?.clone();
    let requested = requested_task.name.clone();
    let mut permanent = BTreeSet::new();
    let mut temporary = BTreeSet::new();
    let mut stack = Vec::new();
    let mut ordered = Vec::new();

    visit(
        task_set,
        &requested,
        &mut permanent,
        &mut temporary,
        &mut stack,
        &mut ordered,
    )?;
    for task in &ordered {
        if !task.os.is_empty() && !task.os.iter().any(|os| os == std::env::consts::OS) {
            return Err(RunnerError::UnsupportedOperatingSystem {
                task: task.name.clone(),
                actual: std::env::consts::OS.to_owned(),
                allowed: task.os.clone(),
            });
        }
    }
    let requested_values = bind(task_set, invocation)?.parameter_values;

    Ok(ExecutionPlan {
        tasks: ordered
            .into_iter()
            .map(|task| BoundTask {
                parameter_values: if task.name == requested {
                    requested_values.clone()
                } else {
                    BTreeMap::new()
                },
                task,
            })
            .collect(),
    })
}

pub(super) fn bind(
    task_set: &TaskSet,
    invocation: &TaskInvocation,
) -> Result<BoundTask, RunnerError> {
    let task = task_set.get(&invocation.task)?.clone();
    let parameter_values = bind_arguments(&task, &invocation.arguments)?;
    Ok(BoundTask {
        task,
        parameter_values,
    })
}

fn bind_arguments(
    task: &Task,
    arguments: &[String],
) -> Result<BTreeMap<String, BoundValue>, RunnerError> {
    let minimum = task
        .parameters
        .iter()
        .filter(|parameter| parameter.default.is_none() && !parameter.variadic)
        .count();
    let maximum = if task.parameters.iter().any(|parameter| parameter.variadic) {
        None
    } else {
        Some(task.parameters.len())
    };
    if arguments.len() < minimum || maximum.is_some_and(|maximum| arguments.len() > maximum) {
        return Err(RunnerError::ArgumentCount {
            task: task.name.clone(),
            minimum,
            maximum,
            actual: arguments.len(),
        });
    }

    let mut values = BTreeMap::new();
    let mut arguments = arguments.iter();
    for parameter in &task.parameters {
        if parameter.variadic {
            let remaining = arguments
                .by_ref()
                .map(|argument| parse_argument(task, parameter, argument))
                .collect::<Result<Vec<_>, _>>()?;
            values.insert(parameter.name.clone(), BoundValue::Variadic(remaining));
        } else if let Some(argument) = arguments.next() {
            values.insert(
                parameter.name.clone(),
                BoundValue::Scalar(parse_argument(task, parameter, argument)?),
            );
        } else if let Some(default) = &parameter.default {
            values.insert(parameter.name.clone(), BoundValue::Scalar(default.clone()));
        }
    }
    Ok(values)
}

fn parse_argument(
    task: &Task,
    parameter: &super::TaskParameter,
    argument: &str,
) -> Result<String, RunnerError> {
    let value = match parameter.kind {
        ScalarKind::Str => Ok(argument.to_owned()),
        ScalarKind::Int => argument
            .parse::<i64>()
            .map(|value| value.to_string())
            .map_err(|_| ()),
        ScalarKind::Float => argument
            .parse::<f64>()
            .map(|value| value.to_string())
            .map_err(|_| ()),
        ScalarKind::Bool => argument
            .parse::<bool>()
            .map(|value| value.to_string())
            .map_err(|_| ()),
    }
    .map_err(|_| RunnerError::InvalidArgument {
        task: task.name.clone(),
        parameter: parameter.name.clone(),
        value: argument.to_owned(),
        kind: parameter.kind,
    })?;
    Ok(value)
}

fn visit(
    task_set: &TaskSet,
    task_name: &str,
    permanent: &mut BTreeSet<String>,
    temporary: &mut BTreeSet<String>,
    stack: &mut Vec<String>,
    ordered: &mut Vec<Task>,
) -> Result<(), RunnerError> {
    if permanent.contains(task_name) {
        return Ok(());
    }
    if temporary.contains(task_name) {
        let start = stack
            .iter()
            .position(|name| name == task_name)
            .expect("temporary task must be on the dependency stack");
        let mut path = stack[start..].to_vec();
        path.push(task_name.to_owned());
        return Err(RunnerError::DependencyCycle { path });
    }

    let task = task_set
        .source_task(task_name)
        .ok_or_else(|| RunnerError::MissingDependency {
            task: stack
                .last()
                .expect("only dependencies may be missing")
                .clone(),
            dependency: task_name.to_owned(),
        })?
        .clone();
    temporary.insert(task.name.clone());
    stack.push(task.name.clone());

    for dependency in &task.dependencies {
        visit(task_set, dependency, permanent, temporary, stack, ordered)?;
    }

    temporary.remove(&task.name);
    stack.pop();
    permanent.insert(task.name.clone());
    ordered.push(task);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::runner::{BoundValue, ScalarKind, Task, TaskInvocation, TaskParameter, TaskSet};

    fn task(name: &str, dependencies: &[&str]) -> Task {
        Task {
            name: name.to_owned(),
            description: None,
            default: false,
            quiet: false,
            private: false,
            group: None,
            confirm: None,
            os: Vec::new(),
            dependencies: dependencies.iter().map(|name| (*name).to_owned()).collect(),
            parameters: Vec::new(),
            environment: BTreeMap::new(),
            cwd: None,
            shell: None,
            commands: Vec::new(),
        }
    }

    fn task_with_parameter(name: &str, parameter: TaskParameter) -> Task {
        let mut task = task(name, &[]);
        task.parameters.push(parameter);
        task
    }

    #[test]
    fn plan_orders_dependencies_before_the_requested_task() {
        let tasks = TaskSet::new(vec![
            task("Deploy", &["Build"]),
            task("Build", &["Generate"]),
            task("Generate", &[]),
        ])
        .unwrap();

        let plan = tasks
            .plan(&TaskInvocation {
                task: "deploy".to_owned(),
                arguments: Vec::new(),
            })
            .unwrap();

        assert_eq!(
            plan.tasks
                .iter()
                .map(|task| task.task.name.as_str())
                .collect::<Vec<_>>(),
            ["Generate", "Build", "Deploy"]
        );
    }

    #[test]
    fn plan_runs_a_shared_dependency_once() {
        let tasks = TaskSet::new(vec![
            task("Deploy", &["Web", "Worker"]),
            task("Web", &["Build"]),
            task("Worker", &["Build"]),
            task("Build", &[]),
        ])
        .unwrap();

        let plan = tasks
            .plan(&TaskInvocation {
                task: "deploy".to_owned(),
                arguments: Vec::new(),
            })
            .unwrap();

        assert_eq!(
            plan.tasks
                .iter()
                .map(|task| task.task.name.as_str())
                .collect::<Vec<_>>(),
            ["Build", "Web", "Worker", "Deploy"]
        );
    }

    #[test]
    fn plan_reports_a_missing_dependency() {
        let tasks = TaskSet::new(vec![task("Deploy", &["Build"])]).unwrap();

        let error = tasks
            .plan(&TaskInvocation {
                task: "deploy".to_owned(),
                arguments: Vec::new(),
            })
            .unwrap_err();

        assert!(matches!(
            error,
            crate::runner::RunnerError::MissingDependency { task, dependency }
                if task == "Deploy" && dependency == "Build"
        ));
    }

    #[test]
    fn plan_reports_a_direct_cycle_with_its_path() {
        let tasks = TaskSet::new(vec![task("A", &["B"]), task("B", &["A"])]).unwrap();

        let error = tasks
            .plan(&TaskInvocation {
                task: "a".to_owned(),
                arguments: Vec::new(),
            })
            .unwrap_err();

        assert!(matches!(
            error,
            crate::runner::RunnerError::DependencyCycle { path }
                if path == ["A", "B", "A"]
        ));
    }

    #[test]
    fn plan_reports_an_indirect_cycle_with_its_path() {
        let tasks = TaskSet::new(vec![
            task("A", &["B"]),
            task("B", &["C"]),
            task("C", &["A"]),
        ])
        .unwrap();

        let error = tasks
            .plan(&TaskInvocation {
                task: "a".to_owned(),
                arguments: Vec::new(),
            })
            .unwrap_err();

        assert!(matches!(
            error,
            crate::runner::RunnerError::DependencyCycle { path }
                if path == ["A", "B", "C", "A"]
        ));
    }

    #[test]
    fn plan_binds_a_requested_string_argument() {
        let tasks = TaskSet::new(vec![task_with_parameter(
            "Deploy",
            TaskParameter {
                name: "environment".to_owned(),
                kind: ScalarKind::Str,
                default: None,
                variadic: false,
            },
        )])
        .unwrap();

        let plan = tasks
            .plan(&TaskInvocation {
                task: "deploy".to_owned(),
                arguments: vec!["staging".to_owned()],
            })
            .unwrap();

        assert_eq!(
            plan.tasks[0].parameter_values,
            BTreeMap::from([(
                "environment".to_owned(),
                BoundValue::Scalar("staging".to_owned())
            )])
        );
    }

    #[test]
    fn plan_converts_each_scalar_argument_to_a_rendered_value() {
        let tasks = TaskSet::new(vec![Task {
            name: "Deploy".to_owned(),
            description: None,
            default: false,
            quiet: false,
            private: false,
            group: None,
            confirm: None,
            os: Vec::new(),
            dependencies: Vec::new(),
            parameters: vec![
                TaskParameter {
                    name: "environment".to_owned(),
                    kind: ScalarKind::Str,
                    default: None,
                    variadic: false,
                },
                TaskParameter {
                    name: "retries".to_owned(),
                    kind: ScalarKind::Int,
                    default: None,
                    variadic: false,
                },
                TaskParameter {
                    name: "ratio".to_owned(),
                    kind: ScalarKind::Float,
                    default: None,
                    variadic: false,
                },
                TaskParameter {
                    name: "dry_run".to_owned(),
                    kind: ScalarKind::Bool,
                    default: None,
                    variadic: false,
                },
            ],
            environment: BTreeMap::new(),
            cwd: None,
            shell: None,
            commands: Vec::new(),
        }])
        .unwrap();

        let plan = tasks
            .plan(&TaskInvocation {
                task: "deploy".to_owned(),
                arguments: vec![
                    "staging".to_owned(),
                    "0042".to_owned(),
                    "2.50".to_owned(),
                    "true".to_owned(),
                ],
            })
            .unwrap();

        assert_eq!(
            plan.tasks[0].parameter_values,
            BTreeMap::from([
                ("dry_run".to_owned(), BoundValue::Scalar("true".to_owned())),
                (
                    "environment".to_owned(),
                    BoundValue::Scalar("staging".to_owned())
                ),
                ("ratio".to_owned(), BoundValue::Scalar("2.5".to_owned())),
                ("retries".to_owned(), BoundValue::Scalar("42".to_owned())),
            ])
        );
    }

    #[test]
    fn plan_rejects_the_wrong_number_of_requested_arguments() {
        let tasks = TaskSet::new(vec![task_with_parameter(
            "Deploy",
            TaskParameter {
                name: "environment".to_owned(),
                kind: ScalarKind::Str,
                default: None,
                variadic: false,
            },
        )])
        .unwrap();

        let error = tasks
            .plan(&TaskInvocation {
                task: "deploy".to_owned(),
                arguments: Vec::new(),
            })
            .unwrap_err();

        assert!(matches!(
            error,
            crate::runner::RunnerError::ArgumentCount {
                task,
                minimum: 1,
                maximum: Some(1),
                actual: 0,
            } if task == "Deploy"
        ));
    }

    #[test]
    fn plan_rejects_an_argument_with_the_wrong_scalar_type() {
        let tasks = TaskSet::new(vec![task_with_parameter(
            "Deploy",
            TaskParameter {
                name: "retries".to_owned(),
                kind: ScalarKind::Int,
                default: None,
                variadic: false,
            },
        )])
        .unwrap();

        let error = tasks
            .plan(&TaskInvocation {
                task: "deploy".to_owned(),
                arguments: vec!["many".to_owned()],
            })
            .unwrap_err();

        assert!(matches!(
            error,
            crate::runner::RunnerError::InvalidArgument {
                task,
                parameter,
                value,
                kind: ScalarKind::Int,
            } if task == "Deploy" && parameter == "retries" && value == "many"
        ));
    }

    #[test]
    fn plan_does_not_consume_arguments_for_dependencies() {
        let tasks = TaskSet::new(vec![
            task("Deploy", &["Configure"]),
            task_with_parameter(
                "Configure",
                TaskParameter {
                    name: "environment".to_owned(),
                    kind: ScalarKind::Str,
                    default: None,
                    variadic: false,
                },
            ),
        ])
        .unwrap();

        let plan = tasks
            .plan(&TaskInvocation {
                task: "deploy".to_owned(),
                arguments: Vec::new(),
            })
            .unwrap();

        assert!(plan.tasks[0].parameter_values.is_empty());
        assert_eq!(plan.tasks[0].task.name, "Configure");
    }

    #[test]
    fn binding_uses_defaults_and_collects_variadic_arguments() {
        let mut deploy = task("Deploy", &[]);
        deploy.parameters = vec![
            TaskParameter {
                name: "environment".to_owned(),
                kind: ScalarKind::Str,
                default: Some("staging".to_owned()),
                variadic: false,
            },
            TaskParameter {
                name: "extra".to_owned(),
                kind: ScalarKind::Str,
                default: None,
                variadic: true,
            },
        ];
        let tasks = TaskSet::new(vec![deploy]).unwrap();

        let bound = tasks
            .bind(&TaskInvocation {
                task: "deploy".to_owned(),
                arguments: vec![
                    "production".to_owned(),
                    "--force".to_owned(),
                    "blue".to_owned(),
                ],
            })
            .unwrap();

        assert_eq!(
            bound.parameter_values,
            BTreeMap::from([
                (
                    "environment".to_owned(),
                    BoundValue::Scalar("production".to_owned()),
                ),
                (
                    "extra".to_owned(),
                    BoundValue::Variadic(vec!["--force".to_owned(), "blue".to_owned()]),
                ),
            ])
        );

        let defaulted = tasks
            .bind(&TaskInvocation {
                task: "deploy".to_owned(),
                arguments: Vec::new(),
            })
            .unwrap();
        assert_eq!(
            defaulted.parameter_values,
            BTreeMap::from([
                (
                    "environment".to_owned(),
                    BoundValue::Scalar("staging".to_owned()),
                ),
                ("extra".to_owned(), BoundValue::Variadic(Vec::new())),
            ])
        );
    }

    #[test]
    fn plan_rejects_a_requested_task_on_an_unsupported_operating_system() {
        let mut deploy = task("Deploy", &[]);
        let unsupported = ["linux", "macos", "windows"]
            .into_iter()
            .find(|candidate| *candidate != std::env::consts::OS)
            .unwrap()
            .to_owned();
        deploy.os = vec![unsupported.clone()];
        let tasks = TaskSet::new(vec![deploy]).unwrap();

        let error = tasks
            .plan(&TaskInvocation {
                task: "deploy".to_owned(),
                arguments: Vec::new(),
            })
            .unwrap_err();

        assert!(matches!(
            error,
            crate::runner::RunnerError::UnsupportedOperatingSystem {
                task,
                actual,
                allowed,
            } if task == "Deploy"
                && actual == std::env::consts::OS
                && allowed == [unsupported]
        ));
    }

    #[test]
    fn plan_rejects_an_unsupported_dependency_before_execution() {
        let deploy = task("Deploy", &["Build"]);
        let mut build = task("Build", &[]);
        let unsupported = ["linux", "macos", "windows"]
            .into_iter()
            .find(|candidate| *candidate != std::env::consts::OS)
            .unwrap()
            .to_owned();
        build.os = vec![unsupported];
        let tasks = TaskSet::new(vec![deploy, build]).unwrap();

        let error = tasks
            .plan(&TaskInvocation {
                task: "deploy".to_owned(),
                arguments: Vec::new(),
            })
            .unwrap_err();

        assert!(matches!(
            error,
            crate::runner::RunnerError::UnsupportedOperatingSystem { task, .. }
                if task == "Build"
        ));
    }

    #[test]
    fn non_variadic_binding_rejects_more_than_the_maximum() {
        let tasks = TaskSet::new(vec![task_with_parameter(
            "Deploy",
            TaskParameter {
                name: "environment".to_owned(),
                kind: ScalarKind::Str,
                default: Some("staging".to_owned()),
                variadic: false,
            },
        )])
        .unwrap();

        let error = tasks
            .bind(&TaskInvocation {
                task: "deploy".to_owned(),
                arguments: vec!["production".to_owned(), "extra".to_owned()],
            })
            .unwrap_err();

        assert!(matches!(
            error,
            crate::runner::RunnerError::ArgumentCount {
                minimum: 0,
                maximum: Some(1),
                actual: 2,
                ..
            }
        ));
    }
}
