//! A chat's input recall cursor. The transcript owns persistence; each browse
//! takes a snapshot so streamed entries and older history pages cannot move it.

#[derive(Default)]
pub struct InputHistory {
    messages: Vec<String>,
    position: Option<usize>,
    draft: String,
}

impl InputHistory {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn active(&self) -> bool {
        self.position.is_some()
    }

    /// `messages` is oldest first and is only consumed when starting a browse.
    pub fn previous(
        &mut self,
        current: &str,
        messages: impl Iterator<Item = String>,
    ) -> Option<String> {
        let position = match self.position {
            Some(position) => position.saturating_sub(1),
            None => {
                self.messages = messages.filter(|text| !text.trim().is_empty()).collect();
                let last = self.messages.len().checked_sub(1)?;
                self.draft = current.to_string();
                last
            }
        };
        self.position = Some(position);
        Some(self.messages[position].clone())
    }

    pub fn next(&mut self) -> Option<String> {
        let position = self.position? + 1;
        if position < self.messages.len() {
            self.position = Some(position);
            Some(self.messages[position].clone())
        } else {
            let draft = std::mem::take(&mut self.draft);
            self.reset();
            Some(draft)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages() -> impl Iterator<Item = String> {
        ["first", "", "  ", "second\n日本語"]
            .into_iter()
            .map(str::to_string)
    }

    #[test]
    fn browses_oldest_to_newest_and_restores_unsent_draft() {
        let mut history = InputHistory::default();
        assert_eq!(history.next(), None);
        assert_eq!(
            history.previous("my draft", messages()).as_deref(),
            Some("second\n日本語")
        );
        assert_eq!(history.previous("", messages()).as_deref(), Some("first"));
        assert_eq!(history.previous("", messages()).as_deref(), Some("first"));
        assert_eq!(history.next().as_deref(), Some("second\n日本語"));
        assert_eq!(history.next().as_deref(), Some("my draft"));
        assert!(!history.active());
        assert_eq!(history.next(), None);
    }

    #[test]
    fn empty_history_does_not_start_browsing() {
        let mut history = InputHistory::default();
        assert_eq!(history.previous("draft", std::iter::empty()), None);
        assert!(!history.active());
    }

    #[test]
    fn snapshot_survives_new_messages_and_older_pages() {
        let mut history = InputHistory::default();
        history.previous("", messages());
        assert_eq!(
            history
                .previous("", ["new page".into()].into_iter())
                .as_deref(),
            Some("first")
        );
        history.reset();
        assert_eq!(
            history.previous("edited recall", messages()).as_deref(),
            Some("second\n日本語")
        );
        assert_eq!(history.next().as_deref(), Some("edited recall"));
    }
}
