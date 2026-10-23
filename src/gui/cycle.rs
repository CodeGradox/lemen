//! ⌃Tab: going through the threads of the worktree on show, the one
//! opened last first, the way ⌘Tab goes through apps. While ⌃ is held each
//! Tab goes one further; letting go of it settles on the thread on show.

use super::{Key, Lemur};
use gpui_kit::*;

/// `threads` by when they were last opened (`recent`, the latest first);
/// ones never opened follow, in their own order.
pub(super) fn by_recent(mut threads: Vec<Key>, recent: &[Key]) -> Vec<Key> {
    threads.sort_by_key(|key| recent.iter().position(|r| r == key).unwrap_or(usize::MAX));
    threads
}

impl Lemur {
    /// The threads of the worktree `active` runs in, or of its host's
    /// threads outside every worktree.
    fn siblings(&self, active: &Key) -> Vec<Key> {
        let (host, session) = active;
        let names: Vec<String> = match self
            .snap
            .worktrees(host)
            .find(|w| w.sessions.iter().any(|s| s.name == *session))
        {
            Some(w) => w.sessions.iter().map(|s| s.name.clone()).collect(),
            None => self
                .snap
                .hosts
                .iter()
                .filter(|h| h.name == *host)
                .flat_map(|h| h.other.iter().map(|s| s.name.clone()))
                .collect(),
        };
        names.into_iter().map(|name| (host.clone(), name)).collect()
    }

    /// Shows the next thread of the worktree on show, or with `back` the
    /// one before, in the order they were last opened.
    pub(super) fn cycle_threads(
        &mut self,
        back: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.prompt.is_some() || self.picker.is_some() {
            return;
        }
        let Some(active) = self.active.clone() else {
            return;
        };
        if self.cycling.is_none() {
            let order = by_recent(self.siblings(&active), &self.recent);
            if order.len() < 2 {
                return;
            }
            let at = order.iter().position(|k| *k == active).unwrap_or(0);
            self.cycling = Some((order, at));
        }
        let Some((order, at)) = &mut self.cycling else {
            return;
        };
        let len = order.len();
        *at = if back {
            (*at + len - 1) % len
        } else {
            (*at + 1) % len
        };
        let (host, session) = order[*at].clone();
        self.open(host, session, window, cx);
        // From the menu, with no ⌃ to let go of.
        if !window.modifiers().control {
            self.end_cycle();
        }
    }

    /// ⌃ came up: the thread on show is the one opened last.
    pub(super) fn end_cycle(&mut self) {
        if self.cycling.take().is_some()
            && let Some(active) = self.active.clone()
        {
            self.touch(active);
        }
    }

    /// Notes `key` as the thread opened last.
    pub(super) fn touch(&mut self, key: Key) {
        self.recent.retain(|k| *k != key);
        self.recent.insert(0, key);
    }
}

#[cfg(test)]
mod tests {
    use super::by_recent;

    #[test]
    fn threads_go_by_when_they_were_opened() {
        let key = |s: &str| ("local".to_string(), s.to_string());
        let threads = vec![key("a"), key("b"), key("c"), key("d")];
        let recent = vec![key("c"), key("x"), key("a")];
        assert_eq!(
            by_recent(threads, &recent),
            [key("c"), key("a"), key("b"), key("d")]
        );
    }
}
