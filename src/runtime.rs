#![allow(dead_code)] // Activated when compiled execution is installed in Task 5.

use crate::compiled::LocalSlot;
use crate::error::{Span, SparError};
use crate::evaluator::ConfigValue;

pub(crate) struct Frame {
    slots: Vec<Option<ConfigValue>>,
}

impl Frame {
    pub fn new(slot_count: usize) -> Self {
        Self {
            slots: vec![None; slot_count],
        }
    }

    pub fn read(&self, slot: LocalSlot, span: &Span) -> Result<&ConfigValue, SparError> {
        self.slots
            .get(slot.0 as usize)
            .ok_or_else(|| internal_slot_error(slot, "is invalid", span))?
            .as_ref()
            .ok_or_else(|| internal_slot_error(slot, "is uninitialized", span))
    }

    pub fn write(
        &mut self,
        slot: LocalSlot,
        value: ConfigValue,
        span: &Span,
    ) -> Result<(), SparError> {
        let destination = self
            .slots
            .get_mut(slot.0 as usize)
            .ok_or_else(|| internal_slot_error(slot, "is invalid", span))?;
        *destination = Some(value);
        Ok(())
    }
}

fn internal_slot_error(slot: LocalSlot, detail: &str, span: &Span) -> SparError {
    SparError::EvalError {
        message: format!("internal runtime error: local slot {} {detail}", slot.0),
        span: span.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_reads_and_writes_valid_slots() {
        let mut frame = Frame::new(1);
        frame
            .write(LocalSlot(0), ConfigValue::Int(7), &Span::dummy())
            .unwrap();
        assert_eq!(
            frame.read(LocalSlot(0), &Span::dummy()).unwrap(),
            &ConfigValue::Int(7)
        );
    }

    #[test]
    fn frame_rejects_invalid_and_uninitialized_slots() {
        let frame = Frame::new(1);
        let uninitialized = frame.read(LocalSlot(0), &Span::dummy()).unwrap_err();
        let invalid = frame.read(LocalSlot(1), &Span::dummy()).unwrap_err();
        assert!(uninitialized
            .to_string()
            .contains("internal runtime error:"));
        assert!(uninitialized.to_string().contains("uninitialized"));
        assert!(invalid.to_string().contains("internal runtime error:"));
        assert!(invalid.to_string().contains("invalid"));
    }
}
