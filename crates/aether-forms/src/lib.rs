//! `aether-forms`: D-RATS form documents, the format messages (ICS-213 and
//! others) travel in.
//!
//! Format (D-Rats @d0e7a39 `formgui.py` `FormFile`, `forms/*.xml`):
//!
//! ```text
//! <xml><form id='ICS213_US_OS'><title>...</title>
//!   <field id='subject'><caption>...</caption><entry type='text'>VALUE</entry></field>
//!   ...
//!   <path><src>CALL</src><dst>CALL</dst><mid>CALL.hex.hex</mid><e>CALL</e>...</path>
//! </form></xml>
//! ```
//!
//! Legacy quirks kept for interop:
//! * field values are XML-escaped **twice** on write (`xml_escape` and then
//!   lxml) and unescaped twice on read, so a literal `&` is `&amp;amp;` in
//!   the file;
//! * an empty value leaves the entry empty (`<entry type='text'/>`);
//! * a choice is stored as `set="y"` on the chosen `<choice>`;
//! * dates are `DD-Mon-YYYY`, times `H:MM:SS`;
//! * message ids are `CALL.<hex(unix - 1114880400)>.<hex(rand 0..100000)>`.
//!
//! Elements this crate doesn't model (`<logo>`, `<att>` attachments, …)
//! are kept verbatim so a received form can be forwarded unchanged.

use std::fmt::Write as _;

/// Bundled templates: (form id, XML).
pub const TEMPLATES: &[(&str, &str)] = &[(
    "ICS213_US_OS",
    include_str!("../templates/ICS213_US_OS.xml"),
)];

/// The ICS-213 general message template id (shared with D-RATS, whose own
/// stylesheet for this id renders our forms).
pub const ICS213: &str = "ICS213_US_OS";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormError {
    Xml(String),
    Structure(String),
    UnknownField(String),
    UnknownTemplate(String),
}

impl std::fmt::Display for FormError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormError::Xml(e) => write!(f, "not well-formed XML: {e}"),
            FormError::Structure(e) => write!(f, "not a D-RATS form: {e}"),
            FormError::UnknownField(id) => write!(f, "the form has no field '{id}'"),
            FormError::UnknownTemplate(id) => write!(f, "no form template '{id}'"),
        }
    }
}

impl std::error::Error for FormError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub text: String,
    pub set: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub id: String,
    pub caption: String,
    /// `text`, `multiline`, `date`, `time`, `numeric`, `choice`,
    /// `multiselect`, `toggle`, `label`, … (kept as written).
    pub kind: String,
    /// Plain value (legacy escaping removed). For choices: the chosen text.
    pub value: String,
    pub choices: Vec<Choice>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormPath {
    pub src: Option<String>,
    pub dst: Option<String>,
    pub mid: Option<String>,
    /// Stations the message has passed through (`<e>`), in order.
    pub hops: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub id: String,
    pub title: String,
    pub fields: Vec<Field>,
    pub path: FormPath,
    /// Unmodelled children of `<form>`, verbatim.
    pub other: Vec<String>,
}

const LEGACY_ESCAPES: [(char, &str); 5] = [
    ('<', "&lt;"),
    ('>', "&gt;"),
    ('&', "&amp;"),
    ('"', "&quot;"),
    ('\'', "&apos;"),
];

/// XML-escape text (one level).
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match LEGACY_ESCAPES.iter().find(|(k, _)| *k == c) {
            Some((_, e)) => out.push_str(e),
            None => out.push(c),
        }
    }
    out
}

/// `formgui.py` `xml_unescape`: replace the five escapes, leave anything
/// else (including unknown `&...;`) as is.
pub fn legacy_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    'outer: while !rest.is_empty() {
        if rest.starts_with('&') {
            for (c, e) in LEGACY_ESCAPES {
                if rest.starts_with(e) {
                    out.push(c);
                    rest = &rest[e.len()..];
                    continue 'outer;
                }
            }
        }
        let ch = rest.chars().next().unwrap_or_default();
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

fn child_text(n: roxmltree::Node<'_, '_>, name: &str) -> Option<String> {
    n.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)
        .map(|c| c.text().unwrap_or("").trim().to_string())
}

impl Form {
    /// Parse a form document. DTDs are refused (no entity expansion).
    pub fn parse(xml: &str) -> Result<Form, FormError> {
        let doc = roxmltree::Document::parse(xml).map_err(|e| FormError::Xml(e.to_string()))?;
        let forms: Vec<_> = doc
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "form")
            .collect();
        let [form] = forms.as_slice() else {
            return Err(FormError::Structure(format!(
                "{} <form> elements",
                forms.len()
            )));
        };
        let id = form
            .attribute("id")
            .ok_or_else(|| FormError::Structure("<form> has no id".into()))?
            .to_string();
        let mut title = None;
        let mut fields = Vec::new();
        let mut path = FormPath::default();
        let mut other = Vec::new();
        for c in form.children().filter(|c| c.is_element()) {
            match c.tag_name().name() {
                "title" => {
                    if title.is_some() {
                        return Err(FormError::Structure("more than one <title>".into()));
                    }
                    title = Some(c.text().unwrap_or("").trim().to_string());
                }
                "field" => {
                    let fid = c
                        .attribute("id")
                        .ok_or_else(|| FormError::Structure("<field> has no id".into()))?;
                    if fields.iter().any(|f: &Field| f.id == fid) {
                        return Err(FormError::Structure(format!("duplicate field id '{fid}'")));
                    }
                    let entry = c
                        .children()
                        .find(|e| e.is_element() && e.tag_name().name() == "entry");
                    let kind = entry
                        .and_then(|e| e.attribute("type"))
                        .unwrap_or("text")
                        .to_string();
                    let choices: Vec<Choice> = entry
                        .map(|e| {
                            e.children()
                                .filter(|x| x.is_element() && x.tag_name().name() == "choice")
                                .map(|x| Choice {
                                    text: x.text().unwrap_or("").trim().to_string(),
                                    set: x.attribute("set") == Some("y"),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let value = if choices.is_empty() {
                        // Direct text of <entry> only (formgui reads entry.text).
                        let t = entry
                            .and_then(|e| e.first_child())
                            .filter(|t| t.is_text())
                            .and_then(|t| t.text())
                            .unwrap_or("");
                        let t = if kind == "multiline" { t } else { t.trim() };
                        legacy_unescape(t.trim())
                    } else {
                        choices
                            .iter()
                            .filter(|x| x.set)
                            .map(|x| x.text.clone())
                            .collect::<Vec<_>>()
                            .join(",")
                    };
                    fields.push(Field {
                        id: fid.to_string(),
                        caption: legacy_unescape(&child_text(c, "caption").unwrap_or_default()),
                        kind,
                        value,
                        choices,
                    });
                }
                "path" => {
                    for p in c.children().filter(|p| p.is_element()) {
                        let v = p.text().unwrap_or("").trim().to_string();
                        if v.is_empty() {
                            continue;
                        }
                        match p.tag_name().name() {
                            "src" => path.src = Some(v),
                            "dst" => path.dst = Some(v),
                            "mid" => path.mid = Some(v),
                            "e" => path.hops.push(v),
                            _ => {}
                        }
                    }
                }
                _ => other.push(xml[c.range()].to_string()),
            }
        }
        Ok(Form {
            id,
            title: title.ok_or_else(|| FormError::Structure("no <title>".into()))?,
            fields,
            path,
            other,
        })
    }

    /// A blank form from a bundled template.
    pub fn from_template(id: &str) -> Result<Form, FormError> {
        let (_, xml) = TEMPLATES
            .iter()
            .find(|(t, _)| *t == id)
            .ok_or_else(|| FormError::UnknownTemplate(id.into()))?;
        Form::parse(xml)
    }

    pub fn get(&self, id: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|f| f.id == id)
            .map(|f| f.value.as_str())
    }

    /// Set a field. Single-line kinds are trimmed, as `formgui` does. For a
    /// choice field the value must be one of its choices.
    pub fn set(&mut self, id: &str, value: &str) -> Result<(), FormError> {
        let f = self
            .fields
            .iter_mut()
            .find(|f| f.id == id)
            .ok_or_else(|| FormError::UnknownField(id.into()))?;
        let value = if f.kind == "multiline" {
            value.to_string()
        } else {
            value.trim().to_string()
        };
        if !f.choices.is_empty() {
            if !value.is_empty() && !f.choices.iter().any(|c| c.text == value) {
                return Err(FormError::Structure(format!(
                    "'{value}' is not a choice of field '{id}'"
                )));
            }
            for c in &mut f.choices {
                c.set = c.text == value;
            }
        }
        f.value = value;
        Ok(())
    }

    fn try_fields(&self, ids: &[&str]) -> Option<String> {
        ids.iter()
            .filter_map(|i| self.get(i))
            .find(|v| !v.is_empty())
            .map(str::to_string)
    }

    /// `FormFile.get_subject_string`.
    pub fn subject(&self) -> String {
        match self.try_fields(&["_auto_subject", "subject"]) {
            Some(s) => s.replace(['\r', '\n'], ""),
            None => format!(
                "{}#{}",
                self.path.src.as_deref().unwrap_or(""),
                self.get("_auto_number").unwrap_or("Unknown")
            ),
        }
    }

    /// `FormFile.get_sender_string`.
    pub fn sender(&self) -> String {
        self.path
            .src
            .clone()
            .or_else(|| self.try_fields(&["_auto_sender", "sender"]))
            .unwrap_or_else(|| "Unknown".into())
    }

    /// `FormFile.get_recipient_string`.
    pub fn recipient(&self) -> String {
        self.path
            .dst
            .clone()
            .or_else(|| self.try_fields(&["_auto_recip", "recip", "recipient"]))
            .unwrap_or_else(|| "Unknown".into())
    }

    /// Write the document in the legacy shape (values double-escaped).
    pub fn to_xml(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "<xml>\n<form id=\"{}\">", escape(&self.id));
        let _ = writeln!(s, "<title>{}</title>", escape(&self.title));
        for f in &self.fields {
            let _ = writeln!(s, "<field id=\"{}\">", escape(&f.id));
            let _ = writeln!(s, "<caption>{}</caption>", escape(&escape(&f.caption)));
            if !f.choices.is_empty() {
                let _ = writeln!(s, "<entry type=\"{}\">", escape(&f.kind));
                for c in &f.choices {
                    let set = if c.set { " set=\"y\"" } else { "" };
                    let _ = writeln!(s, "  <choice{set}>{}</choice>", escape(&c.text));
                }
                s.push_str("</entry>\n");
            } else if f.value.is_empty() {
                let _ = writeln!(s, "<entry type=\"{}\"/>", escape(&f.kind));
            } else {
                let _ = writeln!(
                    s,
                    "<entry type=\"{}\">{}</entry>",
                    escape(&f.kind),
                    escape(&escape(&f.value))
                );
            }
            s.push_str("</field>\n");
        }
        for o in &self.other {
            s.push_str(o);
            s.push('\n');
        }
        let p = &self.path;
        if p.src.is_some() || p.dst.is_some() || p.mid.is_some() || !p.hops.is_empty() {
            s.push_str("<path>");
            for (tag, v) in [("src", &p.src), ("dst", &p.dst), ("mid", &p.mid)] {
                if let Some(v) = v {
                    let _ = write!(s, "<{tag}>{}</{tag}>", escape(v));
                }
            }
            for h in &p.hops {
                let _ = write!(s, "<e>{}</e>", escape(h));
            }
            s.push_str("</path>\n");
        }
        s.push_str("</form>\n</xml>\n");
        s
    }
}

/// Legacy message id: `CALL.<hex(unix_secs - 1114880400)>.<hex(rand)>`,
/// `rand` in 0..=100000 (`ui/main_messages.py` `mkmsgid`).
pub fn message_id(callsign: &str, unix_secs: i64, rand: u32) -> String {
    format!(
        "{}.{:x}.{:x}",
        callsign,
        unix_secs - 1_114_880_400,
        rand % 100_001
    )
}

/// `(year, month 1-12, day)` of a Unix time (UTC).
pub fn utc_ymd(unix_secs: i64) -> (i64, u32, u32) {
    // Howard Hinnant's days-to-civil algorithm.
    let z = unix_secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Date field value, `DD-Mon-YYYY` (formgui `DateWidget`), UTC.
pub fn legacy_date(unix_secs: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (y, m, d) = utc_ymd(unix_secs);
    format!("{d:02}-{}-{y}", MONTHS[(m - 1) as usize])
}

/// Time field value, `H:MM:SS` (formgui `TimeWidget`), UTC.
pub fn legacy_time(unix_secs: i64) -> String {
    let s = unix_secs.rem_euclid(86_400);
    format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A filled ICS-213 in the shape D-RATS 0.4.x writes (reconstructed
    /// from `formgui.py` `xml_escape` + lxml serialisation, not a captured
    /// file; replace with a real capture from C1 testing).
    const LEGACY_FILLED: &str = r#"<?xml version='1.0' encoding='UTF-8'?>
<xml>
<form id='ICS213_US_OS'>
<title>ICS-213 Form</title>
<field id='incident'>
<caption>Incident Name</caption>
<entry type='text'>Flood &amp;amp; Wind</entry>
</field>
<field id='date'>
<caption>Date</caption>
<entry type='date'>05-Oct-2026</entry>
</field>
<field id='subject'>
<caption>Subject</caption>
<entry type='text'>Need cots</entry>
</field>
<field id='message'>
<caption>Message</caption>
<entry type='multiline'>Shelter A needs 40 cots &amp;lt;today&amp;gt;.
Call &amp;quot;Ops&amp;quot;.</entry>
</field>
<logo>logos/ares.png</logo>
<path><e>W4DRS</e><src>W4DRS</src><mid>W4DRS.3a2c9f1.1869f</mid><dst>KK4ODA</dst></path>
</form>
</xml>"#;

    #[test]
    fn reads_legacy_double_escaping_and_path() {
        let f = Form::parse(LEGACY_FILLED).unwrap();
        assert_eq!(f.id, "ICS213_US_OS");
        assert_eq!(f.get("incident"), Some("Flood & Wind"));
        assert_eq!(
            f.get("message"),
            Some("Shelter A needs 40 cots <today>.\nCall \"Ops\".")
        );
        assert_eq!(f.path.src.as_deref(), Some("W4DRS"));
        assert_eq!(f.path.mid.as_deref(), Some("W4DRS.3a2c9f1.1869f"));
        assert_eq!(f.path.hops, vec!["W4DRS"]);
        assert_eq!(f.subject(), "Need cots");
        assert_eq!(f.recipient(), "KK4ODA");
        assert_eq!(f.other, vec!["<logo>logos/ares.png</logo>"]);
    }

    #[test]
    fn writes_what_legacy_reads_and_round_trips() {
        let mut f = Form::from_template(ICS213).unwrap();
        f.set("incident", "  Flood & Wind ").unwrap();
        f.set("message", "Line 1 <b>\nLine 2 'q'").unwrap();
        f.path.src = Some("KK4ODA".into());
        f.path.mid = Some("KK4ODA.1.2".into());
        f.path.hops.push("KK4ODA".into());
        let xml = f.to_xml();
        assert!(xml.contains("<entry type=\"text\">Flood &amp;amp; Wind</entry>"));
        assert!(xml.contains("&amp;lt;b&amp;gt;"));
        assert!(xml.contains("<entry type=\"time\"/>"));
        let back = Form::parse(&xml).unwrap();
        assert_eq!(back, f);
        // Unmodelled elements survive a round trip verbatim.
        let legacy = Form::parse(LEGACY_FILLED).unwrap();
        assert_eq!(Form::parse(&legacy.to_xml()).unwrap(), legacy);
    }

    #[test]
    fn choices_use_the_set_attribute() {
        let xml = "<xml><form id='x'><title>T</title><field id='precedence'><caption>P</caption>\
                   <entry type='choice'><choice set=\"y\">Routine</choice><choice>Priority</choice></entry>\
                   </field></form></xml>";
        let mut f = Form::parse(xml).unwrap();
        assert_eq!(f.get("precedence"), Some("Routine"));
        f.set("precedence", "Priority").unwrap();
        assert!(f.set("precedence", "Flash").is_err());
        let out = f.to_xml();
        assert!(out.contains("<choice set=\"y\">Priority</choice>"));
        assert!(out.contains("<choice>Routine</choice>"));
        assert_eq!(
            Form::parse(&out).unwrap().get("precedence"),
            Some("Priority")
        );
    }

    #[test]
    fn rejects_non_forms_and_entity_tricks() {
        assert!(matches!(
            Form::parse("<xml/>"),
            Err(FormError::Structure(_))
        ));
        assert!(matches!(
            Form::parse("<xml><form id='a'><title/></form><form id='b'><title/></form></xml>"),
            Err(FormError::Structure(_))
        ));
        assert!(matches!(Form::parse("not xml"), Err(FormError::Xml(_))));
        let bomb = "<?xml version='1.0'?><!DOCTYPE x [<!ENTITY a 'aaaa'><!ENTITY b '&a;&a;&a;'>]><xml><form id='x'><title>&b;</title></form></xml>";
        assert!(Form::parse(bomb).is_err(), "DTDs are refused");
        assert!(matches!(
            Form::from_template("nope"),
            Err(FormError::UnknownTemplate(_))
        ));
        let mut f = Form::from_template(ICS213).unwrap();
        assert!(matches!(
            f.set("nope", "x"),
            Err(FormError::UnknownField(_))
        ));
    }

    #[test]
    fn legacy_value_formats() {
        assert_eq!(
            message_id("KK4ODA", 1_114_880_400 + 255, 100_000),
            "KK4ODA.ff.186a0"
        );
        // 2026-10-05T09:05:07Z
        let t = 1_791_191_107;
        assert_eq!(legacy_date(t), "05-Oct-2026");
        assert_eq!(legacy_time(t), "9:05:07");
        assert_eq!(legacy_date(951_782_400), "29-Feb-2000");
        assert_eq!(legacy_date(0), "01-Jan-1970");
    }

    #[test]
    fn unknown_escapes_are_left_alone() {
        assert_eq!(legacy_unescape("a &amp; b &nbsp; &"), "a & b &nbsp; &");
    }
}
