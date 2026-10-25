//! XML as element records: `text.parse_xml()`, `el.select(path)`,
//! `el.select_one(path)` and `el.to_xml()`.
//!
//! Every element is a plain Map `{tag, attrs, text, children}`, so the rest of
//! the language (field access, `filter`, `to_json`, nested assignment) works on
//! it unchanged. Queries run over those Maps, so edited records query too.

use crate::diagnostic::RillError;
use crate::types::{Map, Value};
use std::sync::Arc;

const SHAPE_HELP: &str =
    "an XML element is a record with tag, attrs, text and children, as made by parse_xml";

// ---------------------------------------------------------------- parsing

/// Parse a document into its root element record.
pub fn parse(text: &str) -> Result<Value, RillError> {
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let document = roxmltree::Document::parse_with_options(text, options).map_err(|error| {
        RillError::new(format!("cannot parse XML: {error}"))
            .with_help("the input must be well-formed XML; an XML declaration and a doctype are fine, HTML usually is not")
    })?;
    Ok(element(document.root_element()))
}

fn element(node: roxmltree::Node) -> Value {
    let mut attrs = Map::new();
    let parent_namespaces: Vec<(Option<&str>, &str)> = node
        .parent_element()
        .map(|parent| parent.namespaces().map(|ns| (ns.name(), ns.uri())).collect())
        .unwrap_or_default();
    for namespace in node.namespaces() {
        let key = (namespace.name(), namespace.uri());
        if namespace.name() == Some("xml") || parent_namespaces.contains(&key) {
            continue;
        }
        let name = match namespace.name() {
            Some(prefix) => format!("xmlns:{prefix}"),
            None => "xmlns".to_string(),
        };
        attrs.insert(name, Value::Str(namespace.uri().to_string()));
    }
    for attribute in node.attributes() {
        let name = qualified(node, attribute.namespace(), attribute.name(), true);
        attrs.insert(name, Value::Str(attribute.value().to_string()));
    }
    let text: String =
        node.children().filter(|child| child.is_text()).filter_map(|c| c.text()).collect();
    let children: Vec<Value> =
        node.children().filter(|child| child.is_element()).map(element).collect();
    let tag = qualified(node, node.tag_name().namespace(), node.tag_name().name(), false);
    record(tag, attrs, text.trim().to_string(), children)
}

/// The name as written: `prefix:local` when the namespace has a prefix.
fn qualified(
    node: roxmltree::Node,
    namespace: Option<&str>,
    local: &str,
    attribute: bool,
) -> String {
    let Some(uri) = namespace else { return local.to_string() };
    if uri == "http://www.w3.org/XML/1998/namespace" {
        return format!("xml:{local}");
    }
    // Default-namespace names (and unprefixed attributes) are written bare.
    let _ = attribute;
    match node.lookup_prefix(uri) {
        Some(prefix) if !prefix.is_empty() => format!("{prefix}:{local}"),
        _ => local.to_string(),
    }
}

fn record(tag: String, attrs: Map, text: String, children: Vec<Value>) -> Value {
    let mut map = Map::new();
    map.insert("tag".into(), Value::Str(tag));
    map.insert("attrs".into(), Value::Map(Arc::new(attrs)));
    map.insert("text".into(), Value::Str(text));
    map.insert("children".into(), Value::list(children));
    Value::Map(Arc::new(map))
}

// ---------------------------------------------------------------- element access

fn not_an_element(what: &str, value: &Value) -> RillError {
    RillError::new(format!("{what} needs an XML element, got {}", value.type_name()))
        .with_help(format!("{SHAPE_HELP}; the value is {}", crate::eval::preview_value(value)))
}

/// The element's Map, when `value` is one.
fn as_element(value: &Value) -> Option<&Map> {
    match value {
        Value::Map(map) if matches!(map.get("tag"), Some(Value::Str(_))) => Some(map),
        _ => None,
    }
}

fn tag_of(map: &Map) -> &str {
    match map.get("tag") {
        Some(Value::Str(tag)) => tag,
        _ => "",
    }
}

fn text_of(map: &Map) -> String {
    match map.get("text") {
        None | Some(Value::Null) => String::new(),
        Some(value) => value.as_string_lossy(),
    }
}

fn attr_of(map: &Map, name: &str) -> Option<String> {
    match map.get("attrs") {
        Some(Value::Map(attrs)) => match attrs.get(name) {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_string_lossy()),
        },
        _ => None,
    }
}

fn children_of(map: &Map) -> Vec<&Value> {
    match map.get("children") {
        Some(Value::List(children)) => {
            children.iter().filter(|c| as_element(c).is_some()).collect()
        }
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------- paths

#[derive(Debug)]
enum Test {
    Name(String),
    Any,
    SelfNode,
}

#[derive(Debug)]
enum Predicate {
    HasAttr(String),
    AttrEquals(String, String),
    ChildText(String, String),
    HasChild(String),
    Index(usize),
}

#[derive(Debug)]
struct Step {
    descendant: bool,
    test: Test,
    predicates: Vec<Predicate>,
}

#[derive(Debug)]
enum Terminal {
    Attr(String),
    Text,
}

#[derive(Debug)]
struct Path {
    steps: Vec<Step>,
    terminal: Option<Terminal>,
}

fn path_error(path: &str, at: usize, what: &str) -> RillError {
    RillError::new(format!("bad XML path '{path}': {what} at position {}", at + 1)).with_help(
        "paths are relative to the element: a/b, *, ., //a, a[@k], a[@k='v'], a[child], a[child='v'], a[2], and a trailing @attr or text()",
    )
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')
}

struct Scanner<'a> {
    path: &'a str,
    chars: Vec<char>,
    pos: usize,
}

impl Scanner<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn at(&self, text: &str) -> bool {
        let wanted: Vec<char> = text.chars().collect();
        self.chars[self.pos..].starts_with(&wanted)
    }

    fn eat(&mut self, text: &str) -> bool {
        if self.at(text) {
            self.pos += text.chars().count();
            true
        } else {
            false
        }
    }

    fn name(&mut self) -> Result<String, RillError> {
        let start = self.pos;
        while self.peek().is_some_and(is_name_char) {
            self.pos += 1;
        }
        if start == self.pos {
            return Err(self.error("expected a name"));
        }
        Ok(self.chars[start..self.pos].iter().collect())
    }

    fn quoted(&mut self) -> Result<String, RillError> {
        let Some(quote @ ('\'' | '"')) = self.peek() else {
            return Err(self.error("expected a quoted value"));
        };
        self.pos += 1;
        let start = self.pos;
        while self.peek().is_some_and(|c| c != quote) {
            self.pos += 1;
        }
        if self.peek().is_none() {
            return Err(self.error("unterminated quote"));
        }
        let value = self.chars[start..self.pos].iter().collect();
        self.pos += 1;
        Ok(value)
    }

    fn error(&self, what: &str) -> RillError {
        path_error(self.path, self.pos, what)
    }
}

fn parse_path(path: &str) -> Result<Path, RillError> {
    let mut scanner = Scanner { path, chars: path.trim().chars().collect(), pos: 0 };
    if scanner.chars.is_empty() {
        return Err(path_error(path, 0, "empty path"));
    }
    let mut steps = Vec::new();
    let mut terminal = None;
    let mut descendant = scanner.eat("//");
    if !descendant && scanner.peek() == Some('/') {
        return Err(path_error(path, 0, "a leading / (paths are relative to the element)"));
    }
    loop {
        if terminal.is_some() {
            return Err(scanner.error("nothing may follow @attr or text()"));
        }
        let at_terminal = scanner.at("@") || scanner.at("text()");
        if at_terminal && descendant {
            // `a//@k`: the attribute of `a` or any element below it.
            steps.push(Step { descendant: true, test: Test::SelfNode, predicates: Vec::new() });
        }
        if scanner.eat("@") {
            terminal = Some(Terminal::Attr(scanner.name()?));
        } else if scanner.eat("text()") {
            terminal = Some(Terminal::Text);
        } else {
            let test = if scanner.eat("*") {
                Test::Any
            } else if scanner.peek() == Some('.')
                && !scanner.chars.get(scanner.pos + 1).copied().is_some_and(is_name_char)
            {
                scanner.pos += 1;
                Test::SelfNode
            } else {
                Test::Name(scanner.name()?)
            };
            let mut predicates = Vec::new();
            while scanner.eat("[") {
                let predicate = if scanner.eat("@") {
                    let name = scanner.name()?;
                    if scanner.eat("=") {
                        Predicate::AttrEquals(name, scanner.quoted()?)
                    } else {
                        Predicate::HasAttr(name)
                    }
                } else if scanner.peek().is_some_and(|c| c.is_ascii_digit()) {
                    let start = scanner.pos;
                    while scanner.peek().is_some_and(|c| c.is_ascii_digit()) {
                        scanner.pos += 1;
                    }
                    let digits: String = scanner.chars[start..scanner.pos].iter().collect();
                    match digits.parse::<usize>() {
                        Ok(n) if n >= 1 => Predicate::Index(n),
                        _ => return Err(path_error(path, start, "positions count from 1")),
                    }
                } else {
                    let name = scanner.name()?;
                    if scanner.eat("=") {
                        Predicate::ChildText(name, scanner.quoted()?)
                    } else {
                        Predicate::HasChild(name)
                    }
                };
                if !scanner.eat("]") {
                    return Err(scanner.error("expected ]"));
                }
                predicates.push(predicate);
            }
            steps.push(Step { descendant, test, predicates });
        }
        if scanner.peek().is_none() {
            break;
        }
        if scanner.eat("//") {
            descendant = true;
        } else if scanner.eat("/") {
            descendant = false;
        } else {
            return Err(scanner.error("unexpected character"));
        }
        if scanner.peek().is_none() {
            return Err(scanner.error("the path ends with /"));
        }
    }
    Ok(Path { steps, terminal })
}

fn descendants<'a>(map: &'a Map, out: &mut Vec<&'a Value>) {
    for child in children_of(map) {
        out.push(child);
        if let Some(child) = as_element(child) {
            descendants(child, out);
        }
    }
}

fn matches_test(test: &Test, map: &Map) -> bool {
    match test {
        Test::Name(name) => tag_of(map) == name,
        Test::Any | Test::SelfNode => true,
    }
}

fn apply_step<'a>(step: &Step, context: &'a Value) -> Vec<&'a Value> {
    let map = as_element(context).expect("context is an element");
    let mut candidates: Vec<&Value> = match (&step.test, step.descendant) {
        (Test::SelfNode, false) => vec![context],
        (_, false) => children_of(map),
        (_, true) => {
            let mut all = Vec::new();
            if matches!(step.test, Test::SelfNode) {
                all.push(context);
            }
            descendants(map, &mut all);
            all
        }
    };
    candidates.retain(|value| as_element(value).is_some_and(|m| matches_test(&step.test, m)));
    for predicate in &step.predicates {
        candidates = match predicate {
            Predicate::Index(n) => candidates.get(n - 1).copied().into_iter().collect(),
            _ => candidates
                .into_iter()
                .filter(|value| {
                    let map = as_element(value).expect("candidates are elements");
                    match predicate {
                        Predicate::HasAttr(name) => attr_of(map, name).is_some(),
                        Predicate::AttrEquals(name, wanted) => {
                            attr_of(map, name).as_deref() == Some(wanted.as_str())
                        }
                        Predicate::ChildText(name, wanted) => children_of(map).iter().any(|c| {
                            as_element(c)
                                .is_some_and(|c| tag_of(c) == name && text_of(c) == *wanted)
                        }),
                        Predicate::HasChild(name) => children_of(map)
                            .iter()
                            .any(|c| as_element(c).is_some_and(|c| tag_of(c) == name)),
                        Predicate::Index(_) => unreachable!(),
                    }
                })
                .collect(),
        };
    }
    candidates
}

/// Every match of `path` under `value` (an element or a list of elements).
pub fn select(value: &Value, path: &str, method: &str) -> Result<Vec<Value>, RillError> {
    let path = parse_path(path)?;
    let roots: Vec<&Value> = match value {
        Value::List(items) => items.iter().collect(),
        other => vec![other],
    };
    for root in &roots {
        if as_element(root).is_none() {
            return Err(not_an_element(method, root));
        }
    }
    let mut current = roots;
    for step in &path.steps {
        current = current.into_iter().flat_map(|context| apply_step(step, context)).collect();
    }
    Ok(current
        .into_iter()
        .filter_map(|value| {
            let map = as_element(value)?;
            match &path.terminal {
                None => Some(value.clone()),
                Some(Terminal::Attr(name)) => attr_of(map, name).map(Value::Str),
                Some(Terminal::Text) => Some(Value::Str(text_of(map))),
            }
        })
        .collect())
}

// ---------------------------------------------------------------- writing

/// Indented XML for an element record.
pub fn to_xml(value: &Value) -> Result<String, RillError> {
    let mut out = String::new();
    write_element(value, 0, &mut out)?;
    Ok(out)
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn escape_attr(text: &str) -> String {
    escape_text(text)
        .replace('"', "&quot;")
        .replace('\n', "&#10;")
        .replace('\r', "&#13;")
        .replace('\t', "&#9;")
}

fn write_element(value: &Value, depth: usize, out: &mut String) -> Result<(), RillError> {
    let Some(map) = as_element(value) else {
        return Err(not_an_element("to_xml", value));
    };
    let indent = "  ".repeat(depth);
    let tag = tag_of(map);
    out.push_str(&indent);
    out.push('<');
    out.push_str(tag);
    if let Some(Value::Map(attrs)) = map.get("attrs") {
        for (name, value) in attrs.iter() {
            if matches!(value, Value::Null) {
                continue;
            }
            out.push_str(&format!(" {name}=\"{}\"", escape_attr(&value.as_string_lossy())));
        }
    }
    let text = text_of(map);
    let children = match map.get("children") {
        Some(Value::List(children)) => children.iter().collect::<Vec<_>>(),
        None | Some(Value::Null) => Vec::new(),
        Some(other) => {
            return Err(RillError::new(format!(
                "to_xml: children of <{tag}> must be a List, got {}",
                other.type_name()
            ))
            .with_help(SHAPE_HELP))
        }
    };
    if children.is_empty() {
        if text.is_empty() {
            out.push_str("/>");
        } else {
            out.push_str(&format!(">{}</{tag}>", escape_text(&text)));
        }
        return Ok(());
    }
    out.push_str(">\n");
    if !text.is_empty() {
        out.push_str(&format!("{indent}  {}\n", escape_text(&text)));
    }
    for child in children {
        write_element(child, depth + 1, out)?;
        out.push('\n');
    }
    out.push_str(&format!("{indent}</{tag}>"));
    Ok(())
}
