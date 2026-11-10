//! The browser's session key, held in memory only.
//!
//! A page has no environment to read, so the key a person types has to live somewhere. It lives here
//! and nowhere else: not in `AgentPrefs` (which is saved as text), not in a transcript, not in a
//! document, not in a log. The persisted preference holds only the reserved reference
//! [`SESSION_REFERENCE`](figcraft_agent::SESSION_REFERENCE), and a reload asks for the key again.
//!
//! Two pieces of state are secret and both are redacted from `Debug`: the key itself, and the masked
//! field's buffer while someone is typing it.

use std::cell::RefCell;
use std::rc::Rc;

use figcraft_agent::{Credentials, ProviderError, SESSION_REFERENCE, Secret, SessionCredentials};

/// The session key and the field someone is typing it into.
///
/// Shared by handle: the panel, Preferences and the host all need the same store, and the browser has
/// one thread, so `Rc<RefCell<_>>` is the right shape rather than a lock.
#[derive(Clone)]
pub struct AgentCredentials {
    session: Rc<RefCell<SessionCredentials>>,
    /// What is in the masked field, before it is committed to the store.
    ///
    /// Kept here rather than in `AgentPrefs` so it cannot be serialized by accident: preferences are
    /// written as text, and a draft key in them would be a key on disk.
    draft: Rc<RefCell<String>>,
}

impl Default for AgentCredentials {
    fn default() -> Self {
        Self { session: Rc::new(RefCell::new(SessionCredentials::default())), draft: Rc::new(RefCell::new(String::new())) }
    }
}

impl std::fmt::Debug for AgentCredentials {
    /// Redacted on purpose.
    ///
    /// A `Debug` listing reaches logs, panic messages and bug reports. Neither the key nor the draft
    /// appears here, and neither does their length: a length narrows a guess.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentCredentials").field("is_set", &self.is_set()).finish_non_exhaustive()
    }
}

impl AgentCredentials {
    /// Hold `key` for this session, replacing whatever was there.
    pub fn set_session_key(&mut self, key: impl Into<String>) {
        if let Ok(mut session) = self.session.try_borrow_mut() {
            session.set(key);
        }
    }

    /// Forget the key. An authenticated turn then fails before any request, rather than sending an
    /// empty one.
    pub fn clear_session_key(&mut self) {
        if let Ok(mut session) = self.session.try_borrow_mut() {
            session.clear();
        }
    }

    /// Whether a key is held.
    pub fn is_set(&self) -> bool {
        self.session.try_borrow().map(|s| s.is_set()).unwrap_or(false)
    }

    /// A resolver for a host to construct with.
    ///
    /// The host resolves the reference when it builds a request, through the same contract the desktop
    /// uses, so a key that reaches this store reaches a turn and a connection test alike.
    pub fn resolver(&self) -> SessionResolver {
        SessionResolver { session: Rc::clone(&self.session) }
    }

    /// What the masked field holds, if anything.
    ///
    /// For the field to show its own masked state; the value is never rendered in the clear.
    pub fn draft(&self) -> Option<String> {
        self.draft.try_borrow().ok().map(|d| d.clone()).filter(|d| !d.is_empty())
    }

    /// Replace the masked field's buffer.
    pub fn set_draft(&mut self, draft: impl Into<String>) {
        if let Ok(mut d) = self.draft.try_borrow_mut() {
            *d = draft.into();
        }
    }

    /// Commit the draft to the session store and clear the buffer.
    ///
    /// This is the only path from what someone typed to what a request uses, so an empty draft clears
    /// the key rather than leaving a stale one.
    pub fn commit_draft(&mut self) {
        let draft = self.draft().unwrap_or_default();
        if draft.trim().is_empty() {
            self.clear_session_key();
        } else {
            self.set_session_key(draft.trim());
        }
        self.set_draft(String::new());
    }
}

/// Resolves the browser's session reference against the shared store.
///
/// Refuses a reference it cannot read -- the desktop's environment-variable name, say -- rather than
/// resolving to no key: an endpoint that needs one would otherwise fail with a vaguer message.
#[derive(Clone, Debug)]
pub struct SessionResolver {
    session: Rc<RefCell<SessionCredentials>>,
}

impl Credentials for SessionResolver {
    fn resolve(&self, reference: Option<&str>) -> Result<Option<Secret>, ProviderError> {
        let session = self.session.try_borrow().map_err(|_| ProviderError::credential("the browser session key is busy"))?;
        // A reference that is not ours is refused here, not silently ignored. The value is **not**
        // echoed: if a preferences file or a document ever held a key where a reference belongs, the
        // message would otherwise print it into a log or a bug report.
        if let Some(r) = reference.filter(|r| !r.trim().is_empty())
            && r != SESSION_REFERENCE
        {
            return Err(ProviderError::credential("this build reads a browser-session key, not a named credential"));
        }
        session.resolve(reference)
    }
}

#[cfg(test)]
#[path = "agent_credentials_tests.rs"]
mod tests;
