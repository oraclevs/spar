//! A minimal persistent evaluation session — the foundation `spar repl`
//! (Task 11) is built on, and usable directly by an embedder that wants
//! to evaluate Spar fragments one at a time while keeping prior state.
//!
//! Implementation note: each `eval()` re-compiles and re-evaluates the
//! *entire* accumulated source (all previously committed fragments plus
//! the new one), not just the new fragment. That's what makes rollback
//! trivial and correct — a failing fragment simply never gets appended,
//! so the session's committed source (and the globals last read off of
//! it) are untouched. Executed shell expressions are memoized by their
//! stable source spans so their process effects are not repeated during
//! replay. Other observable effects, such as host calls, still replay.
//! Phase 0 doesn't attempt true incremental (parse-once, extend-in-place)
//! evaluation — that's a materially bigger project than a session
//! foundation needs to be to unblock `spar repl`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::compiler::{CompileOptions, Compiler};
use crate::error::SparError;
use crate::evaluator::ConfigValue;

#[derive(Clone, Default, Debug)]
pub struct EffectLedger(Arc<Mutex<HashMap<(usize, usize), ConfigValue>>>);

impl EffectLedger {
    pub(crate) fn get_or_try_run<E>(
        &self,
        span: (usize, usize),
        run: impl FnOnce() -> Result<ConfigValue, E>,
    ) -> Result<ConfigValue, E> {
        let mut guard = self.0.lock().unwrap();
        if let Some(value) = guard.get(&span) {
            return Ok(value.clone());
        }
        let value = run()?;
        guard.insert(span, value.clone());
        Ok(value)
    }
}

pub struct Session {
    options: CompileOptions,
    committed_source: String,
    globals: HashMap<String, ConfigValue>,
}

impl Session {
    pub(crate) fn new(mut options: CompileOptions) -> Self {
        options.effect_ledger = Some(EffectLedger::default());
        Self {
            options,
            committed_source: String::new(),
            globals: HashMap::new(),
        }
    }

    /// Evaluates `fragment` as if appended to everything previously
    /// committed. On success, commits it — later `eval` calls and
    /// `value` lookups see its declarations and any mutations it made.
    /// On failure, the session is left exactly as it was before this
    /// call; nothing partially commits.
    pub fn eval(&mut self, fragment: &str) -> Result<(), Vec<SparError>> {
        let candidate = if self.committed_source.is_empty() {
            fragment.to_string()
        } else {
            format!("{}\n{}", self.committed_source, fragment)
        };

        let mut options = self.options.clone();
        options.evaluate = true;
        let compilation = Compiler::new(options).compile(&candidate).into_result()?;
        let result = compilation
            .result
            .expect("a successful evaluate:true compile always sets `result`");

        self.committed_source = candidate;
        self.globals = result.globals;
        Ok(())
    }

    /// The current value of a module-scope variable, as of the last
    /// successful `eval`.
    pub fn value(&self, name: &str) -> Option<&ConfigValue> {
        self.globals.get(name)
    }

    /// Every fragment committed so far, concatenated — mainly useful for
    /// diagnostics/debugging a session, not for driving further logic.
    pub fn committed_source(&self) -> &str {
        &self.committed_source
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::host::{HostFunction, HostRegistry};

    #[test]
    fn session_keeps_mutation_and_rejects_failed_fragment_atomically() {
        let mut session = Engine::default().session();
        session.eval("var mut count: int = 1;").unwrap();
        session.eval("count = count + 1;").unwrap();
        assert!(session.eval("count = \"bad\";").is_err());
        assert_eq!(session.value("count"), Some(&ConfigValue::Int(2)));
    }

    #[test]
    fn session_sees_declarations_from_earlier_fragments() {
        let mut session = Engine::default().session();
        session
            .eval("function double(x: int) -> int { return x * 2; };")
            .unwrap();
        session.eval("var y: int = double(x: 21);").unwrap();
        assert_eq!(session.value("y"), Some(&ConfigValue::Int(42)));
    }

    #[test]
    fn session_carries_registered_hosts_across_fragments() {
        let mut hosts = HostRegistry::new();
        hosts
            .register(HostFunction::new(
                "math",
                "square",
                vec![("n", crate::ast::SparType::Int)],
                crate::ast::SparType::Int,
                |args| match &args[0] {
                    ConfigValue::Int(n) => Ok(ConfigValue::Int(n * n)),
                    _ => Err("expected int".to_string()),
                },
            ))
            .unwrap();
        let mut session = Engine::default().with_hosts(hosts).session();
        session.eval("var x: int = math::square(n: 6);").unwrap();
        assert_eq!(session.value("x"), Some(&ConfigValue::Int(36)));
    }

    #[test]
    fn exec_shell_only_runs_once_across_session_replays() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("effect.txt");
        let mut session = Engine::default().session();
        let src = format!(
            r#"function bump() -> int {{
                var r = exec shell {{ printf x >> {:?}; }};
                return 0;
            }};
            var mut triggered: int = bump();"#,
            path.to_string_lossy()
        );

        session.eval(&src).unwrap();
        session.eval("var mut count: int = 1;").unwrap();
        session.eval("count = count + 1;").unwrap();

        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "x",
            "exec shell must only actually run once, not once per replay"
        );
    }
}
