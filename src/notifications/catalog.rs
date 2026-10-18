//! Implementation registry: each notification implementation declares its settings schema and how
//! to turn a [`Message`] into outbound requests (or a script execution). The schema drives both
//! strict write-time validation and the UI form.
use super::{
    events::{EventKind, Message},
    model::{Field, FieldKind, ImplementationKind, ImplementationSchema, SettingValue},
    transport::Outgoing,
};
use crate::api::MediaDomain;
use std::{collections::BTreeMap, sync::OnceLock};

mod library;
mod push;
mod structured;

const BOTH: &[MediaDomain] = &[MediaDomain::Tv, MediaDomain::Movies];
const MAX_TEXT_BYTES: u32 = 512;

impl Field {
    pub(super) fn new(name: &'static str, label: &'static str, kind: FieldKind) -> Self {
        Self {
            name,
            label,
            kind,
            required: false,
            secret: false,
            help: "",
            default: None,
            options: Vec::new(),
            min: None,
            max: None,
            max_length: match kind {
                FieldKind::Text | FieldKind::Url | FieldKind::Path | FieldKind::Select => {
                    Some(MAX_TEXT_BYTES)
                }
                _ => None,
            },
        }
    }
    pub(super) fn text(name: &'static str, label: &'static str) -> Self {
        Self::new(name, label, FieldKind::Text)
    }
    pub(super) fn url(name: &'static str, label: &'static str) -> Self {
        let mut f = Self::new(name, label, FieldKind::Url);
        f.max_length = Some(2048);
        f
    }
    pub(super) fn path(name: &'static str, label: &'static str) -> Self {
        let mut f = Self::new(name, label, FieldKind::Path);
        f.max_length = Some(4096);
        f
    }
    pub(super) fn secret(name: &'static str, label: &'static str) -> Self {
        let mut f = Self::new(name, label, FieldKind::Text);
        f.secret = true;
        f.max_length = Some(1024);
        f
    }
    pub(super) fn number(name: &'static str, label: &'static str, min: i64, max: i64) -> Self {
        let mut f = Self::new(name, label, FieldKind::Number);
        f.min = Some(min);
        f.max = Some(max);
        f
    }
    pub(super) fn boolean(name: &'static str, label: &'static str, default: bool) -> Self {
        let mut f = Self::new(name, label, FieldKind::Boolean);
        f.default = Some(SettingValue::Bool(default));
        f
    }
    pub(super) fn select(
        name: &'static str,
        label: &'static str,
        default: &'static str,
        options: &[&'static str],
    ) -> Self {
        let mut f = Self::new(name, label, FieldKind::Select);
        f.options = options.to_vec();
        f.default = Some(SettingValue::Text(default.into()));
        f
    }
    pub(super) fn required(mut self) -> Self {
        self.required = true;
        self
    }
    pub(super) fn help(mut self, help: &'static str) -> Self {
        self.help = help;
        self
    }
    pub(super) fn default_int(mut self, value: i64) -> Self {
        self.default = Some(SettingValue::Int(value));
        self
    }
    pub(super) fn default_text(mut self, value: &str) -> Self {
        self.default = Some(SettingValue::Text(value.into()));
        self
    }
}

/// Merged view of validated settings and decrypted secrets handed to builders.
pub struct Config<'a> {
    pub settings: &'a BTreeMap<String, SettingValue>,
    pub secrets: &'a BTreeMap<String, String>,
}
impl Config<'_> {
    /// Empty string when unset.
    pub fn text(&self, name: &str) -> &str {
        match self.settings.get(name) {
            Some(SettingValue::Text(v)) => v,
            _ => self.secrets.get(name).map_or("", String::as_str),
        }
    }
    pub fn opt(&self, name: &str) -> Option<&str> {
        Some(self.text(name)).filter(|v| !v.is_empty())
    }
    pub fn int(&self, name: &str) -> Option<i64> {
        match self.settings.get(name) {
            Some(SettingValue::Int(v)) => Some(*v),
            _ => None,
        }
    }
    pub fn bool(&self, name: &str) -> bool {
        matches!(self.settings.get(name), Some(SettingValue::Bool(true)))
    }
    /// Comma separated list, trimmed, empties dropped.
    pub fn list(&self, name: &str) -> Vec<String> {
        self.text(name)
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

pub use library::run_plex as plex;

pub type HttpBuild = fn(&Config, &Message) -> Result<Vec<Outgoing>, &'static str>;

#[derive(Clone, Copy)]
pub enum Build {
    /// Pure request construction; the dispatcher performs the I/O.
    Http(HttpBuild),
    /// Executes a local program (no shell) with the event environment.
    Script,
    /// Plex needs a section lookup before it can refresh, so it has its own flow.
    Plex,
}

pub struct Implementation {
    pub name: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub kind: ImplementationKind,
    pub domains: &'static [MediaDomain],
    pub fields: fn() -> Vec<Field>,
    pub build: Build,
}

fn registry() -> &'static [Implementation] {
    static REGISTRY: OnceLock<Vec<Implementation>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut all = Vec::new();
        all.extend(structured::implementations());
        all.extend(push::implementations());
        all.extend(library::implementations());
        all.sort_by_key(|i| i.name);
        all
    })
}
pub fn find(name: &str) -> Option<&'static Implementation> {
    registry().iter().find(|i| i.name == name)
}
pub fn schemas() -> Vec<ImplementationSchema> {
    registry()
        .iter()
        .map(|i| ImplementationSchema {
            implementation: i.name,
            label: i.label,
            description: i.description,
            kind: i.kind,
            domains: i.domains.to_vec(),
            fields: (i.fields)(),
        })
        .collect()
}

/// Library updaters react only to events that change files on disk.
pub fn changes_library(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::Import
            | EventKind::Rename
            | EventKind::Delete
            | EventKind::FileDelete
            | EventKind::Add
            | EventKind::Test
    )
}

/// Truncates at a character boundary to at most `max` bytes.
pub fn clip(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_owned();
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}
