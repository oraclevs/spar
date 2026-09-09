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

/// Splits `arg` into `(name, value)` on its first `=`, but only if `name`
/// matches one of `task`'s declared parameters — this is what decides
/// whether a call is in named-argument mode at all, so a positional value
/// that merely contains `=` (e.g. `"KEY=value"`) is never mistaken for it.
fn as_named_argument<'a>(task: &Task, arg: &'a str) -> Option<(&'a str, &'a str)> {
    let (name, value) = arg.split_once('=')?;
    task.parameters
        .iter()
        .any(|parameter| parameter.name == name)
        .then_some((name, value))
}

fn bind_arguments(
    task: &Task,
    arguments: &[String],
) -> Result<BTreeMap<String, BoundValue>, RunnerError> {
    if let Some(first) = arguments.first() {
        if as_named_argument(task, first).is_some() {
            let mut pairs = Vec::with_capacity(arguments.len());
            for argument in arguments {
                match argument.split_once('=') {
                    Some((name, value)) => pairs.push((name, value)),
                    None => {
                        return Err(RunnerError::MixedArgumentStyle {
                            task: task.name.clone(),
                            argument: argument.clone(),
                        })
                    }
                }
            }
            return bind_named_arguments(task, &pairs);
        }
    }

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

/// Binds `name=value` pairs to `task`'s parameters, in declaration order,
/// independent of the order the pairs were given in. A variadic parameter
/// collects every pair with its name, in the order given; a defaulted
/// parameter not named in `pairs` falls back to its default, so any prefix
/// of optional parameters can be skipped.
fn bind_named_arguments(
    task: &Task,
    pairs: &[(&str, &str)],
) -> Result<BTreeMap<String, BoundValue>, RunnerError> {
    let mut consumed = vec![false; pairs.len()];
    let mut values = BTreeMap::new();

    for parameter in &task.parameters {
        let matches: Vec<usize> = pairs
            .iter()
            .enumerate()
            .filter(|(_, (name, _))| *name == parameter.name)
            .map(|(index, _)| index)
            .collect();

        if parameter.variadic {
            let bound = matches
                .iter()
                .map(|&index| {
                    consumed[index] = true;
                    parse_argument(task, parameter, pairs[index].1)
                })
                .collect::<Result<Vec<_>, _>>()?;
            values.insert(parameter.name.clone(), BoundValue::Variadic(bound));
        } else if matches.len() > 1 {
            return Err(RunnerError::DuplicateNamedArgument {
                task: task.name.clone(),
                name: parameter.name.clone(),
            });
        } else if let Some(&index) = matches.first() {
            consumed[index] = true;
            values.insert(
                parameter.name.clone(),
                BoundValue::Scalar(parse_argument(task, parameter, pairs[index].1)?),
            );
        } else if let Some(default) = &parameter.default {
            values.insert(parameter.name.clone(), BoundValue::Scalar(default.clone()));
        } else {
            return Err(RunnerError::MissingRequiredArgument {
                task: task.name.clone(),
                name: parameter.name.clone(),
            });
        }
    }

    if let Some(index) = consumed.iter().position(|&seen| !seen) {
        return Err(RunnerError::UnknownNamedArgument {
            task: task.name.clone(),
            name: pairs[index].0.to_owned(),
        });
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
            source_line: None,
            description: None,
            default: false,
            quiet: false,
            private: false,
            group: None,
            confirm: None,
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
            source_line: None,
            description: None,
            default: false,
            quiet: false,
            private: false,
            group: None,
            confirm: None,
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

    fn cpd_task() -> Task {
        let mut task = task("Cpd", &[]);
        task.parameters = vec![
            TaskParameter {
                name: "file".to_owned(),
                kind: ScalarKind::Str,
                default: Some("main.dart".to_owned()),
                variadic: false,
            },
            TaskParameter {
                name: "out".to_owned(),
                kind: ScalarKind::Str,
                default: Some("main".to_owned()),
                variadic: false,
            },
        ];
        task
    }

    #[test]
    fn named_argument_skips_an_earlier_defaulted_parameter() {
        let tasks = TaskSet::new(vec![cpd_task()]).unwrap();

        let bound = tasks
            .bind(&TaskInvocation {
                task: "cpd".to_owned(),
                arguments: vec!["out=result".to_owned()],
            })
            .unwrap();

        assert_eq!(
            bound.parameter_values,
            BTreeMap::from([
                ("file".to_owned(), BoundValue::Scalar("main.dart".to_owned())),
                ("out".to_owned(), BoundValue::Scalar("result".to_owned())),
            ])
        );
    }

    #[test]
    fn named_arguments_may_be_given_out_of_declaration_order() {
        let tasks = TaskSet::new(vec![cpd_task()]).unwrap();

        let bound = tasks
            .bind(&TaskInvocation {
                task: "cpd".to_owned(),
                arguments: vec!["out=result".to_owned(), "file=lib.dart".to_owned()],
            })
            .unwrap();

        assert_eq!(
            bound.parameter_values,
            BTreeMap::from([
                ("file".to_owned(), BoundValue::Scalar("lib.dart".to_owned())),
                ("out".to_owned(), BoundValue::Scalar("result".to_owned())),
            ])
        );
    }

    #[test]
    fn named_argument_with_unknown_parameter_name_is_an_error() {
        let tasks = TaskSet::new(vec![cpd_task()]).unwrap();

        let error = tasks
            .bind(&TaskInvocation {
                task: "cpd".to_owned(),
                arguments: vec!["out=result".to_owned(), "bogus=1".to_owned()],
            })
            .unwrap_err();

        assert!(matches!(
            error,
            crate::runner::RunnerError::UnknownNamedArgument { task, name }
                if task == "Cpd" && name == "bogus"
        ));
    }

    #[test]
    fn named_argument_given_twice_is_an_error() {
        let tasks = TaskSet::new(vec![cpd_task()]).unwrap();

        let error = tasks
            .bind(&TaskInvocation {
                task: "cpd".to_owned(),
                arguments: vec!["out=result".to_owned(), "out=other".to_owned()],
            })
            .unwrap_err();

        assert!(matches!(
            error,
            crate::runner::RunnerError::DuplicateNamedArgument { task, name }
                if task == "Cpd" && name == "out"
        ));
    }

    #[test]
    fn named_arguments_cannot_be_mixed_with_positional_ones() {
        let tasks = TaskSet::new(vec![cpd_task()]).unwrap();

        let error = tasks
            .bind(&TaskInvocation {
                task: "cpd".to_owned(),
                arguments: vec!["out=result".to_owned(), "lib.dart".to_owned()],
            })
            .unwrap_err();

        assert!(matches!(
            error,
            crate::runner::RunnerError::MixedArgumentStyle { task, argument }
                if task == "Cpd" && argument == "lib.dart"
        ));
    }

    #[test]
    fn a_positional_value_that_merely_contains_equals_is_not_treated_as_named() {
        let tasks = TaskSet::new(vec![task_with_parameter(
            "Configure",
            TaskParameter {
                name: "flag".to_owned(),
                kind: ScalarKind::Str,
                default: None,
                variadic: false,
            },
        )])
        .unwrap();

        let bound = tasks
            .bind(&TaskInvocation {
                task: "configure".to_owned(),
                arguments: vec!["KEY=value".to_owned()],
            })
            .unwrap();

        assert_eq!(
            bound.parameter_values,
            BTreeMap::from([("flag".to_owned(), BoundValue::Scalar("KEY=value".to_owned()))])
        );
    }

    #[test]
    fn named_argument_missing_a_required_parameter_is_an_error() {
        let mut configure = task("Configure", &[]);
        configure.parameters = vec![
            TaskParameter {
                name: "environment".to_owned(),
                kind: ScalarKind::Str,
                default: None,
                variadic: false,
            },
            TaskParameter {
                name: "region".to_owned(),
                kind: ScalarKind::Str,
                default: Some("us-east".to_owned()),
                variadic: false,
            },
        ];
        let tasks = TaskSet::new(vec![configure]).unwrap();

        let error = tasks
            .bind(&TaskInvocation {
                task: "configure".to_owned(),
                arguments: vec!["region=eu-west".to_owned()],
            })
            .unwrap_err();

        assert!(matches!(
            error,
            crate::runner::RunnerError::MissingRequiredArgument { task, name }
                if task == "Configure" && name == "environment"
        ));
    }

    #[test]
    fn named_variadic_parameter_collects_repeated_keys() {
        let tasks = TaskSet::new(vec![task_with_parameter(
            "Deploy",
            TaskParameter {
                name: "tags".to_owned(),
                kind: ScalarKind::Str,
                default: None,
                variadic: true,
            },
        )])
        .unwrap();

        let bound = tasks
            .bind(&TaskInvocation {
                task: "deploy".to_owned(),
                arguments: vec![
                    "tags=a".to_owned(),
                    "tags=b".to_owned(),
                    "tags=c".to_owned(),
                ],
            })
            .unwrap();

        assert_eq!(
            bound.parameter_values,
            BTreeMap::from([(
                "tags".to_owned(),
                BoundValue::Variadic(vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]),
            )])
        );
    }
}
