use std::sync::OnceLock;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use regex::Regex;

use crate::archive::ArchiveIndex;
use crate::util::{normalize_member_path, normalize_slashes, sanitize};

#[derive(Debug, Clone)]
pub struct PayloadRef {
    pub kind: String,
    pub raw_value: String,
}

const MD_MAP: [(&str, &str); 6] = [
    ("$(MD_MACROS)", "items\\macro"),
    ("$(MD_IMG)", "items\\picture"),
    ("$(MD_IMAGES)", "items\\picture"),
    ("$(MD_DOCUMENTS)", "items\\document"),
    ("$(MD_DOC)", "items\\document"),
    ("$(MD_MISC)", "items\\misc"),
];

fn attribute_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r#"(?i)(P_ARTICLE_(?:MACRO|GROUPSYMBOLMACRO|PICTUREFILE|EXTERNAL_DOCUMENT_\d+|REF_CONSTRUCTION_NAME|REF_TERMINAL_NAME|ACCESSORYID|IS_ACCESSORY))\s*=\s*"([^"]*)"|'([^']*)'"#,
        )
        .expect("valid payload regex")
    })
}

/// Extracts `P_ARTICLE_*` references from raw XML text.
pub fn extract_from_xml(xml: &str) -> Vec<PayloadRef> {
    let mut refs = Vec::new();
    if xml.trim().is_empty() {
        return refs;
    }

    for captures in attribute_regex().captures_iter(xml) {
        let attribute = captures
            .get(1)
            .map(|value| value.as_str().to_ascii_uppercase())
            .unwrap_or_default();
        let value = captures
            .get(2)
            .or_else(|| captures.get(3))
            .map(|value| value.as_str().trim())
            .unwrap_or("");
        if value.is_empty() {
            continue;
        }

        let kind = match attribute.as_str() {
            "P_ARTICLE_MACRO" => "macro",
            "P_ARTICLE_GROUPSYMBOLMACRO" => "groupsymbolmacro",
            "P_ARTICLE_PICTUREFILE" => "picture",
            "P_ARTICLE_REF_CONSTRUCTION_NAME" => "construction",
            "P_ARTICLE_REF_TERMINAL_NAME" => "connectionpoints",
            "P_ARTICLE_ACCESSORYID" => "accessoryId",
            "P_ARTICLE_IS_ACCESSORY" => "isAccessory",
            value if value.starts_with("P_ARTICLE_EXTERNAL_DOCUMENT_") => "externalDocument",
            _ => continue,
        };

        let raw_value = if kind == "externalDocument" {
            first_line(value)
        } else {
            value.to_string()
        };
        refs.push(PayloadRef {
            kind: kind.to_string(),
            raw_value,
        });
    }

    refs
}

fn first_line(value: &str) -> String {
    let cut = value
        .find("&#10;")
        .or_else(|| value.find('\n'))
        .unwrap_or(value.len());
    value[..cut].trim().to_string()
}

/// Accessory list payload: positions reference parts and placements.
pub fn extract_accessory_list_refs(xml: &str) -> Vec<(String, String)> {
    let mut refs = Vec::new();
    if xml.trim().is_empty() {
        return refs;
    }

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) | Ok(Event::Empty(start)) => {
                if local_name(start.name().as_ref()) == "accessorylistposition" {
                    if let Some(part_nr) = attribute(&start, "partnr") {
                        let part_nr = part_nr.trim();
                        if !part_nr.is_empty() {
                            refs.push(("part".to_string(), part_nr.to_string()));
                        }
                    }
                    if let Some(placement) = attribute(&start, "placement") {
                        let placement = placement.trim();
                        if !placement.is_empty() {
                            refs.push(("placement".to_string(), placement.to_string()));
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => return Vec::new(),
        }
    }
    refs
}

pub fn is_external_uri(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ftp://")
        || lower.starts_with("mailto:")
        || value.starts_with('#')
}

/// Expands EPLAN master-data variables to archive item folders.
pub fn expand_md_variables(raw_value: &str) -> Option<String> {
    let value = raw_value.trim();
    if value.is_empty() || is_external_uri(value) {
        return None;
    }

    let lower = value.to_ascii_lowercase();
    for (token, replacement) in MD_MAP {
        let token_lower = token.to_ascii_lowercase();
        if let Some(index) = lower.find(&token_lower) {
            let mut expanded = String::with_capacity(value.len() + replacement.len());
            expanded.push_str(&value[..index]);
            expanded.push_str(replacement);
            expanded.push_str(&value[index + token.len()..]);
            return Some(normalize_member_path(&expanded));
        }
    }

    Some(normalize_member_path(value))
}

/// Keeps the leading `$(MD_...)` token and sanitizes every non-ASCII character in the tail.
pub fn sanitize_token_tail(raw_value: &str) -> String {
    let normalized = normalize_slashes(raw_value);
    let lower = normalized.to_ascii_lowercase();
    for (token, _) in MD_MAP {
        let token_lower = token.to_ascii_lowercase();
        if let Some(index) = lower.find(&token_lower) {
            let tail = normalized[index + token.len()..].trim_start_matches('\\');
            return format!("{}\\{}", &normalized[..index + token.len()], sanitize(tail));
        }
    }
    sanitize(&normalized)
}

/// Repairs payload references that only resolve through the sanitized archive index.
pub fn rewrite_payload_refs(payload: &str, index: &ArchiveIndex) -> (String, usize) {
    let mut rewrites = 0usize;
    let mut result = payload.to_string();
    let mut seen = std::collections::HashSet::new();

    for payload_ref in extract_from_xml(payload) {
        if !matches!(
            payload_ref.kind.as_str(),
            "macro" | "groupsymbolmacro" | "picture" | "externalDocument"
        ) {
            continue;
        }
        if !seen.insert(payload_ref.raw_value.clone()) {
            continue;
        }
        let Some(expanded) = expand_md_variables(&payload_ref.raw_value) else {
            continue;
        };
        let Some((_, repaired)) = index.try_resolve(&expanded) else {
            continue;
        };
        if !repaired {
            continue;
        }

        let new_value = sanitize_token_tail(&payload_ref.raw_value);
        if new_value == payload_ref.raw_value {
            continue;
        }
        result = result.replace(&payload_ref.raw_value, &new_value);
        rewrites += 1;
    }

    (result, rewrites)
}

fn local_name(raw: &str) -> String {
    let local = match raw.rfind(':') {
        Some(index) => &raw[index + 1..],
        None => raw,
    };
    local.to_string()
}

fn attribute(start: &BytesStart<'_>, name: &str) -> Option<String> {
    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute.ok()?;
        if attribute.key.local_name().as_ref() == name {
            return attribute
                .normalized_value(quick_xml::XmlVersion::Explicit1_0)
                .ok()
                .map(|value| value.into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_attribute_references() {
        let xml = r#"<part P_ARTICLE_PARTNR="X" P_ARTICLE_MACRO="$(MD_MACROS)\A\X.ema" P_ARTICLE_PICTUREFILE='$(MD_IMG)\A\X.png' P_ARTICLE_EXTERNAL_DOCUMENT_2="https://example.com/x&#10;ignored"/>"#;
        let refs = extract_from_xml(xml);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].kind, "macro");
        assert_eq!(refs[0].raw_value, "$(MD_MACROS)\\A\\X.ema");
        assert_eq!(refs[1].kind, "externalDocument");
        assert_eq!(refs[1].raw_value, "https://example.com/x");
    }

    #[test]
    fn expands_master_data_variables() {
        assert_eq!(
            expand_md_variables("$(MD_MACROS)\\A\\X.ema").unwrap(),
            "items\\macro\\A\\X.ema"
        );
        assert_eq!(
            expand_md_variables("$(MD_IMG)/A/X.png").unwrap(),
            "items\\picture\\A\\X.png"
        );
        assert!(expand_md_variables("https://example.com/x").is_none());
    }

    #[test]
    fn reads_accessory_list_positions() {
        let xml = r#"<accessorylist P_PART_ACCESSORYLIST_NAME="LIST"><accessorylistposition partnr="A.1" placement="P1"/><accessorylistposition partnr=" A.2 " /><accessorylistposition placement="P2"/></accessorylist>"#;
        let refs = extract_accessory_list_refs(xml);
        assert_eq!(
            refs,
            vec![
                ("part".to_string(), "A.1".to_string()),
                ("placement".to_string(), "P1".to_string()),
                ("part".to_string(), "A.2".to_string()),
                ("placement".to_string(), "P2".to_string()),
            ]
        );
    }

    #[test]
    fn sanitizes_token_tail() {
        assert_eq!(
            sanitize_token_tail("$(MD_MACROS)\\Schutzgeräte\\X.ema"),
            "$(MD_MACROS)\\Schutzger_te\\X.ema"
        );
    }
}
