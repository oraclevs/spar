use std::fs;
use std::sync::{Arc, Mutex};

use spar::ast::SparType;
use spar::{ConfigValue, Engine, HostFunction, HostRegistry};

fn engine_with_probe(calls: Arc<Mutex<usize>>) -> Engine {
    let mut hosts = HostRegistry::new();
    hosts
        .register(HostFunction::new(
            "probe",
            "touch",
            vec![],
            SparType::Void,
            move |_| {
                *calls.lock().unwrap() += 1;
                Ok(ConfigValue::Int(0))
            },
        ))
        .unwrap();
    Engine::default().with_hosts(hosts)
}

#[test]
fn compile_source_is_reusable_and_does_not_execute() {
    let calls = Arc::new(Mutex::new(0));
    let engine = engine_with_probe(calls.clone());
    let compiled = engine
        .compile_source("function main() -> void { probe::touch(); };")
        .unwrap();

    assert_eq!(*calls.lock().unwrap(), 0);
    assert_eq!(compiled.function_count(), 1);
    assert_eq!(compiled.source_path(), None);
}

#[test]
fn compile_path_retains_source_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.spar");
    fs::write(&path, "function main() -> int { return 7; };").unwrap();

    let compiled = Engine::default().compile_path(&path).unwrap();

    assert_eq!(compiled.source_path(), Some(path.as_path()));
}
