#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputBuffer {
    text: String,
    cursor: usize,
}

impl InputBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_text(text: impl Into<String>) -> Self {
        let text = sanitize_single_line(text.into());
        let cursor = text.len();
        Self { text, cursor }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = sanitize_single_line(text.into());
        self.cursor = self.text.len();
    }

    pub fn insert_char(&mut self, ch: char) -> bool {
        if matches!(ch, '\n' | '\r') {
            return false;
        }

        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        true
    }

    pub fn insert_str(&mut self, text: &str) -> bool {
        let mut changed = false;
        for ch in text.chars() {
            changed |= self.insert_char(ch);
        }
        changed
    }

    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }

        let start = previous_char_boundary(&self.text, self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        true
    }

    pub fn delete(&mut self) -> bool {
        if self.cursor >= self.text.len() {
            return false;
        }

        let end = next_char_boundary(&self.text, self.cursor);
        self.text.replace_range(self.cursor..end, "");
        true
    }

    pub fn move_left(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }

        self.cursor = previous_char_boundary(&self.text, self.cursor);
        true
    }

    pub fn move_right(&mut self) -> bool {
        if self.cursor >= self.text.len() {
            return false;
        }

        self.cursor = next_char_boundary(&self.text, self.cursor);
        true
    }

    pub fn move_home(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }

        self.cursor = 0;
        true
    }

    pub fn move_end(&mut self) -> bool {
        let end = self.text.len();
        if self.cursor == end {
            return false;
        }

        self.cursor = end;
        true
    }
}

fn sanitize_single_line(text: String) -> String {
    text.chars()
        .filter(|ch| !matches!(ch, '\n' | '\r'))
        .collect()
}

fn previous_char_boundary(text: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }

    let prev = text[..cursor]
        .chars()
        .next_back()
        .expect("cursor must be at char boundary");
    cursor - prev.len_utf8()
}

fn next_char_boundary(text: &str, cursor: usize) -> usize {
    if cursor >= text.len() {
        return text.len();
    }

    let next = text[cursor..]
        .chars()
        .next()
        .expect("cursor must be at char boundary");
    cursor + next.len_utf8()
}
