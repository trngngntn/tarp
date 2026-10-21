use std::collections::VecDeque;

use crate::parser::CommandReply;

/// FIFO of in-flight commands awaiting their replies.
pub(crate) struct CommandQueue<T> {
    pending: VecDeque<T>,
}

impl<T> CommandQueue<T> {
    pub(crate) fn new() -> Self {
        Self {
            pending: VecDeque::new(),
        }
    }

    pub(crate) fn push(&mut self, tag: T) {
        self.pending.push_back(tag);
    }

    /// Server-originated replies answer no command, so they pop nothing.
    pub(crate) fn complete(&mut self, reply: &CommandReply) -> Option<T> {
        if reply.from_client {
            self.pending.pop_front()
        } else {
            None
        }
    }

    /// Pops entries from the front for as long as `pred` holds.
    pub(crate) fn pop_front_while(&mut self, mut pred: impl FnMut(&T) -> bool) -> Vec<T> {
        let mut popped = Vec::new();
        while let Some(front) = self.pending.front()
            && pred(front)
        {
            popped.extend(self.pending.pop_front());
        }
        popped
    }

    pub(crate) fn drain(&mut self) -> impl Iterator<Item = T> {
        self.pending.drain(..)
    }
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
