use std::collections::BTreeMap;
use std::process::Command;

pub(super) fn apply(command: &mut Command, environment: &BTreeMap<String, String>) {
    command.envs(environment);
}
