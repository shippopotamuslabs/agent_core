//! Context-reference services (ENC-286 slice 1).
//!
//! Move-verbatim extraction of the `AgentEngine` context-reference surface.
//! These are host-facing surface methods: they acquire the session guard like
//! every other `AgentEngine` entry point (`next_action`, `submit_*`). They
//! never touch the phase machine.

use super::state::SessionHandle;
use super::AgentEngine;
use crate::context_store::{insert_terminal_output, ContextReference};
use crate::events::AgentError;

impl AgentEngine {
    pub fn read_context_reference(
        &self,
        handle: &SessionHandle,
        reference_id: String,
    ) -> Result<String, AgentError> {
        let g = handle.lock_inner();
        g.context_store
            .get(&reference_id)
            .map(|entry| entry.content.clone())
            .ok_or(AgentError::ContextReferenceNotFound { reference_id })
    }

    pub fn search_context_references(
        &self,
        handle: &SessionHandle,
        query: String,
        limit: usize,
    ) -> Vec<crate::context_store::ContextReference> {
        let g = handle.lock_inner();
        g.context_store.search(&query, limit)
    }

    pub fn insert_terminal_output(
        &self,
        handle: &SessionHandle,
        id: String,
        title: String,
        content: String,
    ) -> ContextReference {
        let mut g = handle.lock_inner();
        let turn_index = g.turn_index;
        insert_terminal_output(&mut g.context_store, id, title, content, turn_index)
    }
}
