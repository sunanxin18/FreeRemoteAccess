//! GTK 单入口输入的归属账本；不保存文本，不替代 InputRouter。
//!
//! 调用顺序：begin_key → 释放宿主可变借用 → IM filter（暂存同步 commit）→
//! finish_key → InputRouter/route_input → confirm_physical_press。
//! 只有最后一步确认成功发送，才记录 RemotePhysical。消费过的 press 的归属
//! 决定 release，不能让后续 IM filter 吞掉已经远发的 release。
//!
//! InputRouter 仍独占焦点、修饰键、按键映射和 ReleaseAll。宿主在焦点域、
//! 会话/generation 或 context 生命周期变化时调用 reset_epoch，并 reset IM。
//! 每个 IMContext 回调捕获 bind_im_context 返回的 token；不能在回调发生时
//! 重新绑定以取得新身份。重新绑定须在旧 IM reset/回调解绑之后进行。

use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPhase {
    Press,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOwner {
    RemotePhysical,
    InputMethod,
    Local,
}

/// 只描述 filter 结果；同步 commit 文本由宿主暂存，不进入账本。
#[derive(Debug, Clone, Copy, Default)]
pub struct FilterResult {
    pub consumed: bool,
    pub has_commit: bool,
}

/// 非 Clone、不可外部构造；不借用账本，可跨同步 IM 回调持有。
#[derive(Debug)]
pub struct PendingKeyEvent {
    epoch: Rc<()>,
    id: u64,
    key: Option<u32>,
    phase: KeyPhase,
    repeat: bool,
    remote_eligible: bool,
}

/// 一次发送决定的确认凭据，不能凭硬件键号构造或重放。
#[derive(Debug)]
pub struct PhysicalPressPermit {
    epoch: Rc<()>,
    id: u64,
    key: u32,
}

/// 绑定实际 IMContext 回调生命周期；复制不会刷新身份或授权。
#[derive(Debug, Clone)]
pub struct ImCommitToken {
    epoch: Rc<()>,
    context: Rc<()>,
}

#[derive(Debug)]
pub enum KeyDecision {
    RemotePress {
        hardware_keycode: u32,
        repeat: bool,
        permit: PhysicalPressPermit,
    },
    RemoteRelease {
        hardware_keycode: u32,
    },
    InputMethod {
        accept_sync_commit: bool,
    },
    Local,
    Ignore,
}

#[derive(Debug)]
pub struct KeyOwnershipState {
    epoch: Rc<()>,
    next_id: u64,
    pending_id: Option<u64>,
    decision_id: Option<u64>,
    owners: HashMap<u32, KeyOwner>,
    context: Option<Rc<()>>,
    im_authorized: bool,
}

impl Default for KeyOwnershipState {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyOwnershipState {
    pub fn new() -> Self {
        Self {
            epoch: Rc::new(()),
            next_id: 0,
            pending_id: None,
            decision_id: None,
            owners: HashMap::new(),
            context: None,
            im_authorized: false,
        }
    }

    /// 不产生 ReleaseAll；宿主先处理 InputRouter 的释放，再撤销此账本。
    pub fn reset_epoch(&mut self) {
        self.epoch = Rc::new(());
        self.next_id = 0;
        self.pending_id = None;
        self.decision_id = None;
        self.owners.clear();
        self.context = None;
        self.im_authorized = false;
    }

    /// 新实际 context 或 reset 后的新回调绑定；旧 token 立即失效。
    pub fn bind_im_context(&mut self) -> ImCommitToken {
        let context = Rc::new(());
        self.context = Some(context.clone());
        self.pending_id = None;
        self.decision_id = None;
        self.owners
            .retain(|_, owner| *owner != KeyOwner::InputMethod);
        self.im_authorized = false;
        ImCommitToken {
            epoch: self.epoch.clone(),
            context,
        }
    }

    pub fn owner(&self, hardware_keycode: u32) -> Option<KeyOwner> {
        self.owners.get(&hardware_keycode).copied()
    }

    /// hardware_keycode 为 None 表示映射/来源不受支持，不能据此发送输入。
    /// 新事件使先前未完成的 filter/发送决定失效，不能跨事件迟到确认。
    pub fn begin_key(
        &mut self,
        hardware_keycode: Option<u32>,
        phase: KeyPhase,
        repeat: bool,
        remote_eligible: bool,
    ) -> Option<PendingKeyEvent> {
        self.pending_id = None;
        self.decision_id = None;
        let Some(id) = self.next_id.checked_add(1) else {
            self.reset_epoch();
            return None;
        };
        self.next_id = id;
        self.pending_id = Some(id);
        Some(PendingKeyEvent {
            epoch: self.epoch.clone(),
            id,
            key: hardware_keycode,
            phase,
            repeat,
            remote_eligible,
        })
    }

    pub fn finish_key(&mut self, event: PendingKeyEvent, filter: FilterResult) -> KeyDecision {
        if !Rc::ptr_eq(&event.epoch, &self.epoch) || self.pending_id != Some(event.id) {
            return KeyDecision::Ignore;
        }
        self.pending_id = None;
        let Some(key) = event.key else {
            self.im_authorized = false;
            return KeyDecision::Ignore;
        };
        let owner = self.owner(key);
        if event.phase == KeyPhase::Release {
            self.owners.remove(&key);
            return match owner {
                Some(KeyOwner::RemotePhysical) => KeyDecision::RemoteRelease {
                    hardware_keycode: key,
                },
                Some(KeyOwner::InputMethod) => self.finish_im(filter, false),
                Some(KeyOwner::Local) => KeyDecision::Local,
                None => KeyDecision::Ignore,
            };
        }
        if !event.remote_eligible || owner == Some(KeyOwner::Local) {
            self.im_authorized = false;
            // 不覆盖已远发 press，以保留其 release 责任。
            if owner != Some(KeyOwner::RemotePhysical) {
                self.owners.insert(key, KeyOwner::Local);
            }
            return KeyDecision::Local;
        }
        match owner {
            Some(KeyOwner::RemotePhysical) => {
                self.im_authorized = false;
                if !event.repeat {
                    return KeyDecision::Ignore;
                }
                // 归属已确定为 physical；本次 filter 产生的文本必须丢弃。
                self.press_decision(event.id, key, true)
            }
            Some(KeyOwner::InputMethod) => self.finish_im(filter, true),
            Some(KeyOwner::Local) => unreachable!(),
            None if event.repeat => {
                self.im_authorized = false;
                KeyDecision::Ignore
            }
            None if filter.consumed || filter.has_commit => {
                self.owners.insert(key, KeyOwner::InputMethod);
                self.finish_im(filter, true)
            }
            None => {
                self.im_authorized = false;
                self.press_decision(event.id, key, false)
            }
        }
    }

    fn finish_im(&mut self, filter: FilterResult, may_start_authorization: bool) -> KeyDecision {
        // Release 只能延续原有未消费授权，不能因 filter.consumed 重新授权。
        // 同一约束同时适用于同步 commit 和之后到来的异步 commit。
        let authorized = self.context.is_some()
            && (self.im_authorized
                || (may_start_authorization && (filter.consumed || filter.has_commit)));
        self.im_authorized = authorized && !filter.has_commit;
        KeyDecision::InputMethod {
            accept_sync_commit: authorized && filter.has_commit,
        }
    }

    fn press_decision(&mut self, id: u64, key: u32, repeat: bool) -> KeyDecision {
        self.decision_id = Some(id);
        KeyDecision::RemotePress {
            hardware_keycode: key,
            repeat,
            permit: PhysicalPressPermit {
                epoch: self.epoch.clone(),
                id,
                key,
            },
        }
    }

    /// forwarded 必须是 InputRouter 与 route_input/发送路径的实际接受结果。
    /// 返回 true 表示本次凭据有效；forwarded=false 仍消费凭据但不增加归属。
    pub fn confirm_physical_press(&mut self, permit: PhysicalPressPermit, forwarded: bool) -> bool {
        if !Rc::ptr_eq(&permit.epoch, &self.epoch) || self.decision_id != Some(permit.id) {
            return false;
        }
        self.decision_id = None;
        if forwarded {
            self.owners.insert(permit.key, KeyOwner::RemotePhysical);
        }
        true
    }

    /// 不接收文本；返回 false 时宿主丢弃该 commit。授权一次消费。
    pub fn accept_async_commit(&mut self, token: &ImCommitToken) -> bool {
        let matches = Rc::ptr_eq(&token.epoch, &self.epoch)
            && self
                .context
                .as_ref()
                .is_some_and(|context| Rc::ptr_eq(context, &token.context));
        if !matches || !self.im_authorized || self.pending_id.is_some() {
            return false;
        }
        self.im_authorized = false;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_sequence_revokes_old_identity_instead_of_reusing_it() {
        let mut state = KeyOwnershipState::new();
        let token = state.bind_im_context();
        let old_epoch = state.epoch.clone();
        state.im_authorized = true;
        state.next_id = u64::MAX;
        assert!(state
            .begin_key(Some(38), KeyPhase::Press, false, true)
            .is_none());
        assert!(!Rc::ptr_eq(&old_epoch, &state.epoch));
        assert!(!state.accept_async_commit(&token));
    }
}
