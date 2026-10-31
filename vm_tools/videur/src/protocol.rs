//! Wayland protocol descriptions, read from the XML files the host has
//! installed (wayland's and wayland-protocols').
//!
//! The proxy needs every message's arguments: they say which objects a
//! message creates and how many file descriptors come with it.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    Int,
    Uint,
    Fixed,
    String,
    Object,
    /// The interface of the new object, or None for wl_registry.bind's,
    /// which names it in the message.
    NewId(Option<String>),
    Array,
    Fd,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub name: String,
    pub args: Vec<Arg>,
}

impl Message {
    pub fn fds(&self) -> usize {
        self.args.iter().filter(|a| **a == Arg::Fd).count()
    }
}

#[derive(Debug, Clone)]
pub struct Interface {
    pub name: String,
    pub version: u32,
    pub requests: Vec<Message>,
    pub events: Vec<Message>,
}

#[derive(Debug, Default)]
pub struct Protocols {
    interfaces: Vec<Interface>,
    by_name: HashMap<String, usize>,
}

/// An index into [`Protocols`].
pub type IfaceId = usize;

impl Protocols {
    /// Reads every .xml file under `dirs`, recursively. Missing directories
    /// are skipped.
    pub fn load(dirs: &[&Path]) -> io::Result<Protocols> {
        let mut p = Protocols::default();
        for dir in dirs {
            p.load_dir(dir)?;
        }
        Ok(p)
    }

    fn load_dir(&mut self, dir: &Path) -> io::Result<()> {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                self.load_dir(&path)?;
            } else if path.extension().is_some_and(|e| e == "xml") {
                let text = fs::read_to_string(&path)?;
                let parsed = parse(&text).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{}: {e}", path.display()),
                    )
                })?;
                for iface in parsed {
                    self.add(iface);
                }
            }
        }
        Ok(())
    }

    /// Adds an interface; of two with the same name, the newer wins.
    pub fn add(&mut self, iface: Interface) {
        match self.by_name.get(&iface.name) {
            Some(&i) if self.interfaces[i].version >= iface.version => {}
            Some(&i) => self.interfaces[i] = iface,
            None => {
                self.by_name
                    .insert(iface.name.clone(), self.interfaces.len());
                self.interfaces.push(iface);
            }
        }
    }

    pub fn add_xml(&mut self, text: &str) -> Result<(), String> {
        for iface in parse(text)? {
            self.add(iface);
        }
        Ok(())
    }

    pub fn find(&self, name: &str) -> Option<IfaceId> {
        self.by_name.get(name).copied()
    }

    pub fn get(&self, id: IfaceId) -> &Interface {
        &self.interfaces[id]
    }

    pub fn len(&self) -> usize {
        self.interfaces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.interfaces.is_empty()
    }
}

/// The interfaces in one protocol file.
pub fn parse(text: &str) -> Result<Vec<Interface>, String> {
    let mut out: Vec<Interface> = Vec::new();
    // The message being read, and whether it's an event.
    let mut message: Option<(Message, bool)> = None;
    for tag in Tags::new(text) {
        let tag = tag?;
        match (tag.name, tag.closing) {
            ("interface", false) => {
                let name = tag.attr("name").ok_or("interface without a name")?;
                let version = tag.attr("version").unwrap_or("1");
                let version = version
                    .parse()
                    .map_err(|_| format!("{name}: bad version {version}"))?;
                out.push(Interface {
                    name: name.into(),
                    version,
                    requests: Vec::new(),
                    events: Vec::new(),
                });
            }
            (kind @ ("request" | "event"), false) => {
                let name = tag.attr("name").ok_or("message without a name")?;
                let m = (
                    Message {
                        name: name.into(),
                        args: Vec::new(),
                    },
                    kind == "event",
                );
                if tag.empty {
                    push_message(&mut out, m)?;
                } else {
                    message = Some(m);
                }
            }
            ("request" | "event", true) => {
                if let Some(m) = message.take() {
                    push_message(&mut out, m)?;
                }
            }
            ("arg", false) => {
                let (m, _) = message.as_mut().ok_or("arg outside a message")?;
                let iface = tag.attr("interface").map(str::to_owned);
                let arg = match tag.attr("type").ok_or("arg without a type")? {
                    "int" => Arg::Int,
                    "uint" => Arg::Uint,
                    "fixed" => Arg::Fixed,
                    "string" => Arg::String,
                    "object" => Arg::Object,
                    "new_id" => Arg::NewId(iface),
                    "array" => Arg::Array,
                    "fd" => Arg::Fd,
                    t => return Err(format!("{}: unknown arg type {t}", m.name)),
                };
                m.args.push(arg);
            }
            _ => {}
        }
    }
    Ok(out)
}

fn push_message(out: &mut [Interface], (m, event): (Message, bool)) -> Result<(), String> {
    let iface = out.last_mut().ok_or("message outside an interface")?;
    if event {
        iface.events.push(m)
    } else {
        iface.requests.push(m)
    }
    Ok(())
}

struct Tag<'a> {
    name: &'a str,
    closing: bool,
    /// `<x/>`
    empty: bool,
    attrs: Vec<(&'a str, &'a str)>,
}

impl<'a> Tag<'a> {
    fn attr(&self, name: &str) -> Option<&'a str> {
        self.attrs.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
    }
}

/// The tags of an XML document, ignoring text, comments, CDATA and
/// processing instructions. Enough for protocol files; the attributes the
/// proxy reads (names and types) never contain entities.
struct Tags<'a> {
    rest: &'a str,
}

impl<'a> Tags<'a> {
    fn new(text: &'a str) -> Self {
        Tags { rest: text }
    }

    fn skip_past(&mut self, end: &str) -> Result<(), String> {
        let i = self
            .rest
            .find(end)
            .ok_or_else(|| format!("unterminated, expected {end}"))?;
        self.rest = &self.rest[i + end.len()..];
        Ok(())
    }
}

impl<'a> Iterator for Tags<'a> {
    type Item = Result<Tag<'a>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let start = self.rest.find('<')?;
            self.rest = &self.rest[start..];
            let skipped = if self.rest.starts_with("<!--") {
                self.skip_past("-->")
            } else if self.rest.starts_with("<![CDATA[") {
                self.skip_past("]]>")
            } else if self.rest.starts_with("<?") {
                self.skip_past("?>")
            } else if self.rest.starts_with("<!") {
                self.skip_past(">")
            } else {
                return Some(self.tag());
            };
            if let Err(e) = skipped {
                return Some(Err(e));
            }
        }
    }
}

impl<'a> Tags<'a> {
    fn tag(&mut self) -> Result<Tag<'a>, String> {
        let s = &self.rest[1..];
        let (closing, s) = match s.strip_prefix('/') {
            Some(s) => (true, s),
            None => (false, s),
        };
        let name_end = s
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .ok_or("unterminated tag")?;
        let name = &s[..name_end];
        let mut s = &s[name_end..];
        let mut attrs = Vec::new();
        let empty;
        loop {
            s = s.trim_start();
            if let Some(r) = s.strip_prefix("/>") {
                empty = true;
                s = r;
                break;
            }
            if let Some(r) = s.strip_prefix('>') {
                empty = false;
                s = r;
                break;
            }
            let eq = s
                .find('=')
                .ok_or_else(|| format!("<{name}>: bad attribute"))?;
            let key = s[..eq].trim();
            let r = s[eq + 1..].trim_start();
            let quote = r
                .chars()
                .next()
                .filter(|c| *c == '"' || *c == '\'')
                .ok_or_else(|| format!("<{name}>: unquoted attribute"))?;
            let r = &r[1..];
            let end = r
                .find(quote)
                .ok_or_else(|| format!("<{name}>: unterminated attribute"))?;
            attrs.push((key, &r[..end]));
            s = &r[end + 1..];
        }
        self.rest = s;
        Ok(Tag {
            name,
            closing,
            empty,
            attrs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_messages_and_args() {
        let xml = r#"<?xml version="1.0"?>
<protocol name="t">
  <!-- a <comment> -->
  <interface name="a" version="3">
    <description summary="x &amp; y">text with <![CDATA[ <odd> ]]></description>
    <request name="destroy" type="destructor"/>
    <request name="make">
      <arg name="id" type="new_id" interface="b" summary="it's 'quoted' > here"/>
      <arg name="fd" type="fd"/>
    </request>
    <event name="done"><arg name="v" type="uint"/></event>
  </interface>
  <interface name="b" version="1"/>
</protocol>"#;
        let ifaces = parse(xml).unwrap();
        assert_eq!(ifaces.len(), 2);
        let a = &ifaces[0];
        assert_eq!((a.name.as_str(), a.version), ("a", 3));
        assert_eq!(a.requests.len(), 2);
        assert_eq!(
            a.requests[1].args,
            vec![Arg::NewId(Some("b".into())), Arg::Fd]
        );
        assert_eq!(a.requests[1].fds(), 1);
        assert_eq!(a.events[0].args, vec![Arg::Uint]);
    }

    #[test]
    fn reads_the_installed_core_protocol() {
        let p = Protocols::load(&[Path::new("/usr/share/wayland")]).unwrap();
        let registry = p.get(p.find("wl_registry").unwrap());
        assert_eq!(registry.requests[0].args, vec![Arg::Uint, Arg::NewId(None)]);
        let offer = p.get(p.find("wl_data_offer").unwrap());
        assert_eq!(offer.requests[1].name, "receive");
        assert_eq!(offer.requests[1].fds(), 1);
    }
}
