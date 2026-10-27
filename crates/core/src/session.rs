//! Session: the live document plus undo steps and the operator log.
//! Undo is snapshot based, like Blender's global (memfile) undo.

use anyhow::{Result, anyhow};

use crate::data::Document;
use crate::lang;
use crate::ops;
use crate::query;

const UNDO_STEPS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Human,
    Agent,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Human => "human",
            Source::Agent => "agent",
        }
    }
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub seq: u64,
    pub source: Source,
    pub text: String,
    pub error: Option<String>,
    merge_key: Option<String>,
}

/// The document is private: the only way to change it is [`Session::run`],
/// so every edit (human or agent) is an operator in the log.
#[derive(Default)]
pub struct Session {
    doc: Document,
    /// Frame the human is looking at. Not part of undo.
    pub frame: f32,
    pub selected: Option<String>,
    /// Composition open in the timeline and viewer (None: the film). `frame`
    /// counts from its start. Not part of undo.
    pub comp: Option<String>,
    /// Bumped on every change; the UI and renderers use it to invalidate.
    pub version: u64,
    pub log: Vec<LogEntry>,
    undo: Vec<Document>,
    redo: Vec<Document>,
    seq: u64,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn doc(&self) -> &Document {
        &self.doc
    }

    fn push_log(&mut self, source: Source, text: &str, error: Option<String>, merge_key: Option<String>) {
        self.seq += 1;
        self.log.push(LogEntry { seq: self.seq, source, text: text.trim().into(), error, merge_key });
    }

    /// Runs a script as one atomic undo step. On any error nothing changes.
    pub fn run(&mut self, script: &str, source: Source) -> Result<usize> {
        self.run_merged(script, source, None)
    }

    /// Like [`run`](Self::run), but consecutive runs with the same `merge_key`
    /// (e.g. one slider drag) collapse into a single undo step and log entry.
    pub fn run_merged(&mut self, script: &str, source: Source, merge_key: Option<&str>) -> Result<usize> {
        let ops = match lang::parse(script) {
            Ok(ops) => ops,
            Err(e) => {
                self.push_log(source, script, Some(format!("{e:#}")), None);
                return Err(e);
            }
        };
        let before = self.doc.clone();
        for (line, op) in &ops {
            if let Err(e) = ops::apply(&mut self.doc, op) {
                self.doc = before;
                let e = anyhow!("line {line}: {e:#} (nothing applied)");
                self.push_log(source, script, Some(e.to_string()), None);
                return Err(e);
            }
        }
        let merge = merge_key.is_some()
            && self.log.last().is_some_and(|l| l.source == source && l.merge_key.as_deref() == merge_key);
        if merge {
            self.log.last_mut().unwrap().text = script.trim().into();
        } else {
            self.undo.push(before);
            if self.undo.len() > UNDO_STEPS {
                self.undo.remove(0);
            }
            self.push_log(source, script, None, merge_key.map(Into::into));
        }
        self.redo.clear();
        self.version += 1;
        Ok(ops.len())
    }

    pub fn undo(&mut self, source: Source) -> bool {
        let Some(prev) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(&mut self.doc, prev));
        self.push_log(source, "undo", None, None);
        self.version += 1;
        true
    }

    pub fn redo(&mut self, source: Source) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(&mut self.doc, next));
        self.push_log(source, "redo", None, None);
        self.version += 1;
        true
    }

    pub fn query(&self, selector: &str) -> Result<String> {
        let mut out = query::query(&self.doc, selector, self.frame)?;
        if matches!(selector.trim(), "" | "*") {
            if let Some(s) = &self.selected {
                out = out.replacen('\n', &format!(", selected {s}\n"), 1);
            }
        }
        Ok(out)
    }

    /// Log entries after `since`, optionally only from one source.
    pub fn log_since(&self, since: u64, only: Option<Source>) -> String {
        self.log
            .iter()
            .filter(|l| l.seq > since && only.is_none_or(|s| s == l.source))
            .map(|l| match &l.error {
                Some(e) => format!("#{} {} ERR {}: {}", l.seq, l.source.name(), e, l.text),
                None => format!("#{} {}: {}", l.seq, l.source.name(), l.text),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        std::fs::write(path, lang::dump(&self.doc) + "\n")?;
        Ok(())
    }

    pub fn load(&mut self, path: &std::path::Path) -> Result<()> {
        let text = std::fs::read_to_string(path)?;
        let mut doc = Document::default();
        for (line, op) in lang::parse(&text)? {
            ops::apply(&mut doc, &op).map_err(|e| anyhow!("{}:{line}: {e:#}", path.display()))?;
        }
        *self = Session { doc, ..Session::default() };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_script_is_atomic() {
        let mut s = Session::new();
        s.run("add cube a", Source::Agent).unwrap();
        assert!(s.run("add cube b\nset nope.loc 1", Source::Agent).is_err());
        assert!(!s.doc.objects.contains_key("b"));
        assert!(s.undo(Source::Human));
        assert!(s.doc.objects.is_empty());
    }

    #[test]
    fn drags_merge_into_one_step() {
        let mut s = Session::new();
        s.run("add cube a", Source::Agent).unwrap();
        for x in 1..5 {
            s.run_merged(&format!("set a.loc.x {x}"), Source::Human, Some("drag a.loc")).unwrap();
        }
        assert_eq!(s.log.len(), 2);
        s.undo(Source::Human);
        assert_eq!(s.doc.objects["a"].loc.x, 0.0);
    }
}
