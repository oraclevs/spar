use std::collections::BTreeMap;
use std::path::PathBuf;

use super::RunnerError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub name: String,
    pub description: Option<String>,
    pub default: bool,
    pub quiet: bool,
    pub dependencies: Vec<String>,
    pub parameters: Vec<TaskParameter>,
    pub environment: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub commands: Vec<TaskCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCommand {
    pub template: CommandTemplate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandTemplate {
    pub parts: Vec<TemplatePart>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplatePart {
    Literal(String),
    Parameter(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarKind {
    Str,
    Int,
    Float,
    Bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskParameter {
    pub name: String,
    pub kind: ScalarKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskInvocation {
    pub task: String,
    pub arguments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSet {
    tasks: BTreeMap<String, Task>,
    cli_names: BTreeMap<String, String>,
}

impl TaskSet {
    pub fn new(tasks: Vec<Task>) -> Result<Self, RunnerError> {
        let mut catalog: BTreeMap<String, Task> = BTreeMap::new();
        let mut cli_names: BTreeMap<String, String> = BTreeMap::new();

        for task in tasks {
            let name = task.name.clone();
            if catalog.contains_key(&name) {
                return Err(RunnerError::DuplicateTaskName { name });
            }

            let cli_name = name.to_lowercase();
            if let Some(first) = cli_names.get(&cli_name) {
                return Err(RunnerError::CliNameCollision {
                    first: first.clone(),
                    second: name,
                    cli_name,
                });
            }
            cli_names.insert(cli_name, name.clone());
            catalog.insert(name, task);
        }

        Ok(Self {
            tasks: catalog,
            cli_names,
        })
    }

    pub fn default_task(&self) -> Result<&Task, RunnerError> {
        let defaults: Vec<&Task> = self.tasks.values().filter(|task| task.default).collect();
        let Some(task) = defaults.first() else {
            return Err(RunnerError::MissingDefaultTask);
        };
        if defaults.len() > 1 {
            return Err(RunnerError::MultipleDefaultTasks {
                names: defaults.iter().map(|task| task.name.clone()).collect(),
            });
        }
        Ok(*task)
    }

    pub fn get(&self, name: &str) -> Result<&Task, RunnerError> {
        let cli_name = name.to_lowercase();
        let Some(source_name) = self.cli_names.get(&cli_name) else {
            return Err(RunnerError::UnknownTask {
                name: name.to_owned(),
            });
        };
        Ok(&self.tasks[source_name])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn task(name: &str) -> Task {
        Task {
            name: name.to_owned(),
            description: None,
            default: false,
            quiet: false,
            dependencies: Vec::new(),
            parameters: Vec::new(),
            environment: BTreeMap::new(),
            cwd: None,
            commands: Vec::new(),
        }
    }

    #[test]
    fn duplicate_task_names_fail() {
        let error = TaskSet::new(vec![task("Build"), task("Build")]).unwrap_err();

        assert!(matches!(error, RunnerError::DuplicateTaskName { name } if name == "Build"));
    }

    #[test]
    fn no_default_task_fails() {
        let tasks = TaskSet::new(vec![task("Build")]).unwrap();

        assert!(matches!(
            tasks.default_task(),
            Err(RunnerError::MissingDefaultTask)
        ));
    }

    #[test]
    fn multiple_default_tasks_fail() {
        let mut build = task("Build");
        build.default = true;
        let mut deploy = task("Deploy");
        deploy.default = true;
        let mut test = task("Test");
        test.default = true;
        let tasks = TaskSet::new(vec![build, deploy, test]).unwrap();

        assert!(matches!(
            tasks.default_task(),
            Err(RunnerError::MultipleDefaultTasks { names })
                if names == ["Build", "Deploy", "Test"]
        ));
    }

    #[test]
    fn unknown_requested_task_fails() {
        let tasks = TaskSet::new(vec![task("Build")]).unwrap();

        assert!(matches!(
            tasks.get("missing"),
            Err(RunnerError::UnknownTask { name }) if name == "missing"
        ));
    }

    #[test]
    fn colliding_lowercase_cli_names_fail() {
        let error = TaskSet::new(vec![task("Build"), task("BUILD")]).unwrap_err();

        assert!(matches!(
            error,
            RunnerError::CliNameCollision {
                first,
                second,
                cli_name,
            } if first == "Build" && second == "BUILD" && cli_name == "build"
        ));
    }

    #[test]
    fn lookup_normalizes_cli_name_to_lowercase() {
        let tasks = TaskSet::new(vec![task("Build")]).unwrap();

        assert_eq!(tasks.get("BUILD").unwrap().name, "Build");
    }
}
