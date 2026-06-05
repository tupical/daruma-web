//! Per-task relation-count map shared between `TaskList` and `TaskRow`.
//!
//! `TaskList` populates the map once per task-batch reload by calling
//! `POST /v1/relations/query`; `TaskRow` reads it to render the
//! blocker/blocked-by indicator chips in the collapsed row.

use leptos::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use taskagent_domain::{Relation, RelationKind};
use taskagent_shared::TaskId;

/// Per-task aggregate counts derived from a flat list of `Relation`s.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelationCounts {
    pub blocks: u32,
    pub blocked_by: u32,
    pub relates_to: u32,
    pub duplicates: u32,
    pub duplicated_by: u32,
}

/// Reactive map exposed via Leptos context.
///
/// `Arc<HashMap<…>>` lets the row component clone cheaply and lookup by key
/// without holding a write lock on the signal.
#[derive(Clone, Copy)]
pub struct RelationsCtx {
    pub counts: RwSignal<Arc<HashMap<TaskId, RelationCounts>>>,
}

impl RelationsCtx {
    pub fn new() -> Self {
        Self {
            counts: RwSignal::new(Arc::new(HashMap::new())),
        }
    }

    /// Replace the map with counts aggregated from `rels`, relative to each
    /// task in `task_ids`.  Tasks with zero relations remain absent from the
    /// map (lookups fall through to `RelationCounts::default()` at the call
    /// site, which is what we want).
    ///
    /// `done_ids` is the set of tasks currently in `Status::Done`.  Blocks
    /// relations are skipped whenever either endpoint is Done — the blocker
    /// has already lifted, or the blocked task has been resolved anyway.
    /// `RelatesTo` and `Duplicates` are unaffected: they're informational and
    /// remain visible after closure.
    pub fn set_from_relations(
        &self,
        task_ids: &[TaskId],
        done_ids: &HashSet<TaskId>,
        rels: &[Relation],
    ) {
        let in_scope: HashSet<TaskId> = task_ids.iter().copied().collect();
        let mut map: HashMap<TaskId, RelationCounts> = HashMap::new();

        for r in rels {
            // Skip Blocks relations that no longer gate progress.
            let drop_blocks = matches!(r.kind, RelationKind::Blocks)
                && (done_ids.contains(&r.from) || done_ids.contains(&r.to));
            if drop_blocks {
                continue;
            }

            // `WasBlocking` is historical and does not contribute to active
            // relation indicators.
            if matches!(r.kind, RelationKind::WasBlocking) {
                continue;
            }

            // For each endpoint that belongs to the visible task set, bump the
            // matching counter relative to that endpoint's perspective.
            for (endpoint, is_from) in [(r.from, true), (r.to, false)] {
                if !in_scope.contains(&endpoint) {
                    continue;
                }
                let entry = map.entry(endpoint).or_default();
                match r.kind {
                    RelationKind::Blocks => {
                        if is_from {
                            entry.blocks += 1;
                        } else {
                            entry.blocked_by += 1;
                        }
                    }
                    RelationKind::RelatesTo => {
                        entry.relates_to += 1;
                    }
                    RelationKind::Duplicates => {
                        if is_from {
                            entry.duplicates += 1;
                        } else {
                            entry.duplicated_by += 1;
                        }
                    }
                    RelationKind::WasBlocking => {} // filtered above
                }
            }
        }

        self.counts.set(Arc::new(map));
    }
}
