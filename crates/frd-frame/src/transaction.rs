use std::time::Instant;

use frd_core::{PixelSize, SessionId};

use crate::{EnqueuedSurfaceUpdate, FrameCompleteness, PixelFormat, PixelPatch, SurfaceUpdate};

#[derive(Debug)]
pub struct FrameReset {
    pub session_id: SessionId,
    pub generation: u64,
    pub size: PixelSize,
    pub format: PixelFormat,
}

#[derive(Debug)]
pub struct FrameRevision {
    pub session_id: SessionId,
    pub generation: u64,
    pub revision: u64,
    pub patches: Vec<PixelPatch>,
    pub completeness: FrameCompleteness,
}

#[derive(Debug)]
pub enum FrameTransaction {
    Startup {
        earliest_constituent_enqueue_at: Instant,
        reset: FrameReset,
        revision: FrameRevision,
    },
    Revision {
        earliest_constituent_enqueue_at: Instant,
        revision: FrameRevision,
    },
}

impl FrameTransaction {
    pub fn earliest_constituent_enqueue_at(&self) -> Instant {
        match self {
            Self::Startup {
                earliest_constituent_enqueue_at,
                ..
            }
            | Self::Revision {
                earliest_constituent_enqueue_at,
                ..
            } => *earliest_constituent_enqueue_at,
        }
    }

    pub fn source_update_count(&self) -> usize {
        match self {
            Self::Startup { .. } => 3,
            Self::Revision { .. } => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameTransactionError {
    InvalidReset,
    ForeignSession,
    StaleReset,
    UpdateBeforeReset,
    StaleUpdate,
    DuplicateDamage,
    RevisionWhilePending,
    BoundaryWithoutDamage,
    BoundaryMismatch,
    StartupBoundaryNotFullBaseline,
}

#[derive(Clone, Copy, Debug)]
struct CompilerSurfaceState {
    generation: u64,
    last_revision: u64,
}

struct PendingStartup {
    reset: FrameReset,
    earliest_constituent_enqueue_at: Instant,
    damage: Option<PendingDamage>,
}

struct PendingDamage {
    generation: u64,
    revision: u64,
    patches: Vec<PixelPatch>,
    earliest_constituent_enqueue_at: Instant,
}

/// 将按到达顺序排空的表面更新编译为只含完整修订的事务。
pub struct FrameTransactionCompiler {
    session_id: SessionId,
    active: Option<CompilerSurfaceState>,
    pending_startup: Option<PendingStartup>,
    pending_revision: Option<PendingDamage>,
}

impl FrameTransactionCompiler {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            active: None,
            pending_startup: None,
            pending_revision: None,
        }
    }

    pub fn compile<I>(&mut self, updates: I) -> Result<Vec<FrameTransaction>, FrameTransactionError>
    where
        I: IntoIterator<Item = EnqueuedSurfaceUpdate>,
    {
        let mut transactions = Vec::new();
        for envelope in updates {
            if surface_update_session_id(&envelope.update) != self.session_id {
                return Err(FrameTransactionError::ForeignSession);
            }

            match envelope.update {
                SurfaceUpdate::Reset {
                    session_id,
                    generation,
                    size,
                    format,
                } => {
                    self.compile_reset(envelope.enqueued_at, session_id, generation, size, format)?
                }
                SurfaceUpdate::Damage {
                    generation,
                    revision,
                    patches,
                    ..
                } => self.compile_damage(envelope.enqueued_at, generation, revision, patches)?,
                SurfaceUpdate::FrameBoundary {
                    generation,
                    revision,
                    completeness,
                    ..
                } => {
                    if let Some(transaction) = self.compile_boundary(
                        envelope.enqueued_at,
                        generation,
                        revision,
                        completeness,
                    )? {
                        transactions.push(transaction);
                    }
                }
            }
        }
        Ok(transactions)
    }

    pub fn has_buffered_input(&self) -> bool {
        self.pending_startup.is_some() || self.pending_revision.is_some()
    }

    pub fn buffered_source_update_count(&self) -> usize {
        let startup_count = self
            .pending_startup
            .as_ref()
            .map_or(0, |startup| 1 + usize::from(startup.damage.is_some()));
        startup_count + usize::from(self.pending_revision.is_some())
    }

    pub fn earliest_buffered_enqueue_at(&self) -> Option<Instant> {
        let startup_at = self
            .pending_startup
            .as_ref()
            .map(|startup| startup.earliest_constituent_enqueue_at);
        let revision_at = self
            .pending_revision
            .as_ref()
            .map(|revision| revision.earliest_constituent_enqueue_at);
        match (startup_at, revision_at) {
            (Some(startup_at), Some(revision_at)) => Some(startup_at.min(revision_at)),
            (Some(at), None) | (None, Some(at)) => Some(at),
            (None, None) => None,
        }
    }

    fn compile_reset(
        &mut self,
        enqueued_at: Instant,
        session_id: SessionId,
        generation: u64,
        size: PixelSize,
        format: PixelFormat,
    ) -> Result<(), FrameTransactionError> {
        if generation == 0 || size.width == 0 || size.height == 0 {
            return Err(FrameTransactionError::InvalidReset);
        }
        if let Some(current_generation) = self.current_generation() {
            if generation <= current_generation {
                return Err(FrameTransactionError::StaleReset);
            }
        }

        self.pending_startup = Some(PendingStartup {
            reset: FrameReset {
                session_id,
                generation,
                size,
                format,
            },
            earliest_constituent_enqueue_at: enqueued_at,
            damage: None,
        });
        self.pending_revision = None;
        Ok(())
    }

    fn compile_damage(
        &mut self,
        enqueued_at: Instant,
        generation: u64,
        revision: u64,
        patches: Vec<PixelPatch>,
    ) -> Result<(), FrameTransactionError> {
        if let Some(startup) = self.pending_startup.as_mut() {
            if generation != startup.reset.generation || revision == 0 {
                return Err(FrameTransactionError::StaleUpdate);
            }
            if let Some(pending) = startup.damage.as_ref() {
                return Err(if revision == pending.revision {
                    FrameTransactionError::DuplicateDamage
                } else {
                    FrameTransactionError::RevisionWhilePending
                });
            }
            startup.earliest_constituent_enqueue_at =
                startup.earliest_constituent_enqueue_at.min(enqueued_at);
            startup.damage = Some(PendingDamage {
                generation,
                revision,
                patches,
                earliest_constituent_enqueue_at: enqueued_at,
            });
            return Ok(());
        }

        let Some(active) = self.active else {
            return Err(FrameTransactionError::UpdateBeforeReset);
        };
        if generation != active.generation || revision == 0 || revision <= active.last_revision {
            return Err(FrameTransactionError::StaleUpdate);
        }
        if let Some(pending) = self.pending_revision.as_ref() {
            return Err(if revision == pending.revision {
                FrameTransactionError::DuplicateDamage
            } else {
                FrameTransactionError::RevisionWhilePending
            });
        }

        self.pending_revision = Some(PendingDamage {
            generation,
            revision,
            patches,
            earliest_constituent_enqueue_at: enqueued_at,
        });
        Ok(())
    }

    fn compile_boundary(
        &mut self,
        enqueued_at: Instant,
        generation: u64,
        revision: u64,
        completeness: FrameCompleteness,
    ) -> Result<Option<FrameTransaction>, FrameTransactionError> {
        if let Some(startup) = self.pending_startup.as_ref() {
            if generation != startup.reset.generation {
                return Err(FrameTransactionError::StaleUpdate);
            }
            let Some(damage) = startup.damage.as_ref() else {
                return Err(FrameTransactionError::BoundaryWithoutDamage);
            };
            if revision != damage.revision || generation != damage.generation {
                return Err(FrameTransactionError::BoundaryMismatch);
            }
            if completeness != FrameCompleteness::FullBaseline {
                return Err(FrameTransactionError::StartupBoundaryNotFullBaseline);
            }

            let PendingStartup {
                reset,
                earliest_constituent_enqueue_at,
                damage: Some(damage),
            } = self.pending_startup.take().expect("已验证待启动状态")
            else {
                unreachable!("已验证待启动损伤存在")
            };
            let earliest_constituent_enqueue_at = earliest_constituent_enqueue_at
                .min(damage.earliest_constituent_enqueue_at)
                .min(enqueued_at);
            self.active = Some(CompilerSurfaceState {
                generation,
                last_revision: revision,
            });
            return Ok(Some(FrameTransaction::Startup {
                earliest_constituent_enqueue_at,
                reset,
                revision: FrameRevision {
                    session_id: self.session_id,
                    generation: damage.generation,
                    revision: damage.revision,
                    patches: damage.patches,
                    completeness,
                },
            }));
        }

        let Some(active) = self.active else {
            return Err(FrameTransactionError::UpdateBeforeReset);
        };
        if generation != active.generation || revision == 0 || revision <= active.last_revision {
            return Err(FrameTransactionError::StaleUpdate);
        }
        let Some(pending) = self.pending_revision.as_ref() else {
            return Err(FrameTransactionError::BoundaryWithoutDamage);
        };
        if generation != pending.generation || revision != pending.revision {
            return Err(FrameTransactionError::BoundaryMismatch);
        }

        let pending = self.pending_revision.take().expect("已验证待修订存在");
        let earliest_constituent_enqueue_at =
            pending.earliest_constituent_enqueue_at.min(enqueued_at);
        self.active = Some(CompilerSurfaceState {
            generation,
            last_revision: revision,
        });
        Ok(Some(FrameTransaction::Revision {
            earliest_constituent_enqueue_at,
            revision: FrameRevision {
                session_id: self.session_id,
                generation: pending.generation,
                revision: pending.revision,
                patches: pending.patches,
                completeness,
            },
        }))
    }

    fn current_generation(&self) -> Option<u64> {
        self.active
            .map(|active| active.generation)
            .into_iter()
            .chain(
                self.pending_startup
                    .as_ref()
                    .map(|startup| startup.reset.generation),
            )
            .max()
    }
}

fn surface_update_session_id(update: &SurfaceUpdate) -> SessionId {
    match update {
        SurfaceUpdate::Reset { session_id, .. }
        | SurfaceUpdate::Damage { session_id, .. }
        | SurfaceUpdate::FrameBoundary { session_id, .. } => *session_id,
    }
}
