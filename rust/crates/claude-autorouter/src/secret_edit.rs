//! No-output terminal editing for a single hidden readline question. There is
//! no completion, persistent history, or terminal rendering of secret bytes.
//! Controls follow Node 22's readline interface (historySize: 0):
//! https://github.com/nodejs/node/blob/v22.14.0/lib/internal/readline/interface.js
use std::collections::VecDeque;

pub enum Action {
    Continue,
    Line(String),
    Cancel,
    Suspend,
}
#[derive(Default)]
pub struct Editor {
    line: Vec<char>,
    cursor: usize,
    pending: Vec<u8>,
    undo: VecDeque<(Vec<char>, usize)>,
    redo: Vec<(Vec<char>, usize)>,
    kills: VecDeque<Vec<char>>,
    kill_index: usize,
    yanking: bool,
}
impl Editor {
    pub fn escape_pending(&self) -> bool {
        self.pending.first() == Some(&27)
    }
    pub fn expire_escape(&mut self) {
        self.pending.clear();
    }
    pub fn eof(&mut self) -> Action {
        if !self.pending.is_empty() && !self.escape_pending() {
            let tail = String::from_utf8_lossy(&self.pending).into_owned();
            self.pending.clear();
            self.insert(&tail.chars().collect::<Vec<_>>());
        }
        if self.line.is_empty() {
            Action::Cancel
        } else {
            Action::Line(self.line.iter().collect())
        }
    }
    pub fn feed(&mut self, bytes: &[u8]) -> Action {
        self.pending.extend_from_slice(bytes);
        while !self.pending.is_empty() {
            if self.escape_pending() {
                let Some((consumed, key, modifier)) = escape(&self.pending) else {
                    // A terminal sequence is short. Discard an unrecognized
                    // oversized sequence without retaining or displaying it.
                    if self.pending.len() > 4096 {
                        self.pending.clear();
                    }
                    return Action::Continue;
                };
                self.pending.drain(..consumed);
                self.special(key, modifier);
                continue;
            }
            let (character, size) = match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    let character = text.chars().next().expect("nonempty input");
                    (character, character.len_utf8())
                }
                Err(error) if error.valid_up_to() != 0 => {
                    let text = std::str::from_utf8(&self.pending[..error.valid_up_to()])
                        .expect("valid UTF-8 prefix");
                    let character = text.chars().next().expect("nonempty prefix");
                    (character, character.len_utf8())
                }
                Err(error) => match error.error_len() {
                    Some(size) => ('\u{fffd}', size),
                    None => return Action::Continue,
                },
            };
            self.pending.drain(..size);
            self.yanking = false;
            match character {
                '\r' | '\n' => return Action::Line(self.line.iter().collect()),
                '\u{3}' => return Action::Cancel,
                '\u{4}' if self.line.is_empty() => return Action::Cancel,
                '\u{4}' => self.delete(self.cursor, (self.cursor + 1).min(self.line.len())),
                '\u{1a}' => return Action::Suspend,
                '\u{1}' => self.cursor = 0,
                '\u{5}' => self.cursor = self.line.len(),
                '\u{2}' => self.cursor = self.cursor.saturating_sub(1),
                '\u{6}' => self.cursor = (self.cursor + 1).min(self.line.len()),
                '\u{8}' | '\u{7f}' => self.delete(self.cursor.saturating_sub(1), self.cursor),
                '\u{15}' => self.kill(0, self.cursor),
                '\u{b}' => self.kill(self.cursor, self.line.len()),
                '\u{17}' => self.delete(self.word_left(), self.cursor),
                '\u{19}' => self.yank(),
                '\u{1f}' => self.undo(),
                '\u{1e}' => self.redo(),
                // Readline ignores other control keys. Tab has no completer
                // and is inserted literally, just as the baseline prompt.
                '\t' => self.insert(&['\t']),
                '\u{0}'..='\u{1f}' => {}
                character => self.insert(&[character]),
            }
        }
        Action::Continue
    }
    fn before_edit(&mut self) {
        self.undo.push_back((self.line.clone(), self.cursor));
        if self.undo.len() > 2048 {
            self.undo.pop_front();
        }
    }
    fn insert(&mut self, text: &[char]) {
        self.before_edit();
        self.line
            .splice(self.cursor..self.cursor, text.iter().copied());
        self.cursor += text.len();
    }
    fn delete(&mut self, from: usize, to: usize) {
        if from < to {
            self.before_edit();
            self.line.drain(from..to);
            self.cursor = from;
        }
    }
    fn kill(&mut self, from: usize, to: usize) {
        self.before_edit();
        let deleted: Vec<_> = self.line.drain(from..to).collect();
        self.cursor = from;
        if !deleted.is_empty() && self.kills.front() != Some(&deleted) {
            self.kills.push_front(deleted);
            self.kills.truncate(32);
            self.kill_index = 0;
        }
    }
    fn yank(&mut self) {
        if let Some(text) = self.kills.get(self.kill_index).cloned() {
            self.insert(&text);
            self.yanking = true;
        }
    }
    fn yank_pop(&mut self) {
        if !self.yanking || self.kills.len() < 2 {
            return;
        }
        let length = self.kills[self.kill_index].len();
        self.kill_index = (self.kill_index + 1) % self.kills.len();
        let replacement = &self.kills[self.kill_index];
        let start = self.cursor.saturating_sub(length);
        self.line
            .splice(start..self.cursor, replacement.iter().copied());
        self.cursor = start + replacement.len();
    }
    fn undo(&mut self) {
        if let Some((line, cursor)) = self.undo.pop_back() {
            self.redo
                .push((std::mem::replace(&mut self.line, line), self.cursor));
            self.cursor = cursor;
        }
    }
    fn redo(&mut self) {
        if let Some((line, cursor)) = self.redo.pop() {
            self.before_edit();
            self.line = line;
            self.cursor = cursor;
        }
    }
    fn word_left(&self) -> usize {
        let mut index = self.cursor;
        while index > 0 && space(self.line[index - 1]) {
            index -= 1;
        }
        if index > 0 {
            let word = word(self.line[index - 1]);
            while index > 0
                && !space(self.line[index - 1])
                && word == self::word(self.line[index - 1])
            {
                index -= 1;
            }
        }
        index
    }
    fn word_right(&self, deleting: bool) -> usize {
        let mut index = self.cursor;
        if index < self.line.len() {
            let first = self.line[index];
            while index < self.line.len()
                && if space(first) {
                    space(self.line[index])
                } else if word(first) {
                    word(self.line[index])
                } else {
                    !word(self.line[index]) && (deleting || !space(self.line[index]))
                }
            {
                index += 1;
            }
            while index < self.line.len() && space(self.line[index]) {
                index += 1;
            }
        }
        index
    }
    fn special(&mut self, key: Key, modifier: Modifier) {
        if !(modifier.meta && matches!(key, Key::Character('y'))) {
            self.yanking = false;
        }
        if modifier.control && modifier.shift {
            match key {
                Key::Backspace => self.kill(0, self.cursor),
                Key::Delete => self.kill(self.cursor, self.line.len()),
                _ => {}
            }
        } else if modifier.control || modifier.meta {
            match key {
                Key::Left if modifier.control => self.cursor = self.word_left(),
                Key::Right if modifier.control => self.cursor = self.word_right(false),
                Key::Character('b') if modifier.meta => self.cursor = self.word_left(),
                Key::Character('f') if modifier.meta => self.cursor = self.word_right(false),
                Key::Character('d') if modifier.meta => {
                    self.delete(self.cursor, self.word_right(true))
                }
                Key::Character('y') if modifier.meta => self.yank_pop(),
                Key::Backspace => self.delete(self.word_left(), self.cursor),
                Key::Delete => self.delete(self.cursor, self.word_right(true)),
                _ => {}
            }
        } else {
            match key {
                Key::Left => self.cursor = self.cursor.saturating_sub(1),
                Key::Right => self.cursor = (self.cursor + 1).min(self.line.len()),
                Key::Home => self.cursor = 0,
                Key::End => self.cursor = self.line.len(),
                Key::Delete => self.delete(self.cursor, (self.cursor + 1).min(self.line.len())),
                Key::Backspace => self.delete(self.cursor.saturating_sub(1), self.cursor),
                _ => {}
            }
        }
    }
}
fn word(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}
fn space(character: char) -> bool {
    autorouter_core::config::js_trim(&character.to_string()).is_empty()
}
#[derive(Clone, Copy)]
enum Key {
    Left,
    Right,
    Home,
    End,
    Backspace,
    Delete,
    Character(char),
    Unknown,
}
#[derive(Default)]
struct Modifier {
    control: bool,
    meta: bool,
    shift: bool,
}
fn escape(bytes: &[u8]) -> Option<(usize, Key, Modifier)> {
    let second = *bytes.get(1)?;
    if second == 27 {
        return Some((1, Key::Unknown, Modifier::default()));
    }
    if !matches!(second, b'[' | b'O') {
        let key = match second {
            8 | 127 => Key::Backspace,
            character if character.is_ascii() => {
                Key::Character(char::from(character).to_ascii_lowercase())
            }
            _ => Key::Unknown,
        };
        let size = if second.is_ascii() {
            2
        } else {
            match std::str::from_utf8(&bytes[1..]) {
                Ok(value) => 1 + value.chars().next()?.len_utf8(),
                Err(error) if error.valid_up_to() > 0 => 1 + error.valid_up_to(),
                Err(error) => 1 + error.error_len()?,
            }
        };
        return Some((
            size,
            key,
            Modifier {
                meta: true,
                ..Default::default()
            },
        ));
    }
    let mut end = 2;
    // Linux-console function keys use an extra '['. They have no action in a
    // history-free hidden prompt, but consume the complete terminal sequence.
    if bytes.get(end) == Some(&b'[') {
        end += 1;
    }
    while let Some(byte) = bytes.get(end) {
        if (0x40..=0x7e).contains(byte) {
            break;
        }
        end += 1;
    }
    let final_byte = *bytes.get(end)?;
    let parameter = std::str::from_utf8(&bytes[2..end]).unwrap_or_default();
    let mut parts = parameter.split(';');
    let first = parts.next().unwrap_or_default();
    let bits = parts
        .next()
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(1)
        .saturating_sub(1);
    let modifier = Modifier {
        shift: bits & 1 != 0,
        meta: bits & 2 != 0,
        control: bits & 4 != 0,
    };
    let key = match final_byte {
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'~' => match first {
            "1" | "7" => Key::Home,
            "4" | "8" => Key::End,
            "3" => Key::Delete,
            _ => Key::Unknown,
        },
        _ => Key::Unknown,
    };
    Some((end + 1, key, modifier))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_edits_preserve_unicode_and_never_insert_escape_controls() {
        for (input, expected) in [
            ("synthetic-abc\u{1b}[DZ\n", "synthetic-abZc"),
            ("a😀b\u{1b}[D\u{7f}Z\r", "aZb"),
            ("abc\u{1b}[H\u{1b}[3~Z\u{1b}[FQ\n", "ZbcQ"),
            ("one two\u{17}three\n", "one three"),
            ("one two\u{1b}bX\n", "one Xtwo"),
            ("one two\u{1}\u{1b}dX\n", "Xtwo"),
            ("abc\u{15}Z\u{19}\n", "Zabc"),
            ("abc\u{1}\u{b}Z\u{19}\n", "Zabc"),
            ("ab\u{1f}Z\n", "aZ"),
            ("ab\u{1f}\u{1e}\n", "ab"),
            ("ab\u{1b}[A\u{1b}[B\u{14}\n", "ab"),
            ("a\u{1b}[200~bc\u{1b}[201~\n", "abc"),
        ] {
            for chunk in [1, 2, 7, usize::MAX] {
                let mut editor = Editor::default();
                let mut result = None;
                for bytes in input.as_bytes().chunks(chunk) {
                    if let Action::Line(value) = editor.feed(bytes) {
                        result = Some(value);
                    }
                }
                assert_eq!(result.as_deref(), Some(expected));
            }
        }
    }
    #[test]
    fn eof_interrupt_and_suspend_are_distinct() {
        assert!(matches!(Editor::default().feed(b"\x03"), Action::Cancel));
        assert!(matches!(Editor::default().feed(b"\x04"), Action::Cancel));
        assert!(matches!(Editor::default().feed(b"\x1a"), Action::Suspend));
        let mut editor = Editor::default();
        assert!(matches!(editor.feed(b"abc\x04"), Action::Continue));
        assert!(matches!(editor.eof(), Action::Line(value) if value == "abc"));
        let mut editor = Editor::default();
        editor.feed(b"\x1b");
        editor.expire_escape();
        assert!(matches!(editor.feed(b"safe\n"), Action::Line(value) if value == "safe"));
    }
}
