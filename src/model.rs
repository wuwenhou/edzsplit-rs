use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::error::{AppError, Result};
use crate::util::{decode_text, item_member_path};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemRef {
    pub item_type: String,
    pub name: String,
    pub locator: String,
}

impl ItemRef {
    pub fn member_path(&self) -> String {
        item_member_path(&self.item_type, &self.locator)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Package {
    pub package_type: String,
    pub key: String,
    pub name: String,
    pub items: Vec<ItemRef>,
}

#[derive(Debug, Clone, Default)]
pub struct Manifest {
    pub version: String,
    pub packages: Vec<Package>,
}

impl Manifest {
    pub fn parse_bytes(bytes: &[u8]) -> Result<Self> {
        Self::parse(&decode_text(bytes))
    }

    pub fn parse(xml: &str) -> Result<Self> {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(false);

        let mut manifest = Manifest {
            version: "2.0".to_string(),
            packages: Vec::new(),
        };
        let mut stack: Vec<String> = Vec::new();
        let mut current_package: Option<Package> = None;
        let mut current_item: Option<ItemRef> = None;
        let mut seen_root = false;

        loop {
            let event = reader.read_event().map_err(|error| {
                AppError::message(format!("manifest.xml: invalid XML: {error}"))
            })?;
            match event {
                Event::Start(start) => {
                    let name = local_name(start.name().as_ref());
                    if stack.is_empty() {
                        if !name.eq_ignore_ascii_case("manifest") {
                            return Err(AppError::message(
                                "manifest.xml: root element is not <manifest>.",
                            ));
                        }
                        seen_root = true;
                        manifest.version =
                            attribute(&start, "version").unwrap_or_else(|| "2.0".to_string());
                    } else {
                        self_start_element(
                            &name,
                            &start,
                            &stack,
                            &mut current_package,
                            &mut current_item,
                        );
                    }
                    stack.push(name);
                }
                Event::Empty(start) => {
                    let name = local_name(start.name().as_ref());
                    if stack.is_empty() {
                        if !name.eq_ignore_ascii_case("manifest") {
                            return Err(AppError::message(
                                "manifest.xml: root element is not <manifest>.",
                            ));
                        }
                        seen_root = true;
                        manifest.version =
                            attribute(&start, "version").unwrap_or_else(|| "2.0".to_string());
                    } else {
                        self_start_element(
                            &name,
                            &start,
                            &stack,
                            &mut current_package,
                            &mut current_item,
                        );
                    }
                    if name == "package"
                        && stack.len() == 2
                        && let Some(package) = current_package.take()
                    {
                        manifest.packages.push(package);
                    } else if name == "item"
                        && stack.len() == 4
                        && let (Some(package), Some(item)) =
                            (current_package.as_mut(), current_item.take())
                    {
                        package.items.push(item);
                    }
                }
                Event::End(end) => {
                    let name = local_name(end.name().as_ref());
                    let depth = stack.len();
                    if depth == 5
                        && name == "item"
                        && stack.last().is_some_and(|n| n == "item")
                        && let (Some(package), Some(item)) =
                            (current_package.as_mut(), current_item.take())
                    {
                        package.items.push(item);
                    } else if depth == 3
                        && name == "package"
                        && stack.last().is_some_and(|n| n == "package")
                        && let Some(package) = current_package.take()
                    {
                        manifest.packages.push(package);
                    }
                    stack.pop();
                }
                Event::Eof => break,
                _ => {}
            }
        }

        if !seen_root {
            return Err(AppError::message("manifest.xml: missing root element."));
        }

        Ok(manifest)
    }

    pub fn to_xml(&self) -> Result<String> {
        let mut out = String::with_capacity(256 + self.packages.len() * 256);
        out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n");
        out.push_str("<manifest");
        push_attribute(&mut out, "version", &self.version)?;
        out.push_str(">\r\n");

        if self.packages.is_empty() {
            out.push_str(" <packages />\r\n");
        } else {
            out.push_str(" <packages>\r\n");
            for package in &self.packages {
                out.push_str("  <package");
                push_attribute(&mut out, "type", &package.package_type)?;
                push_attribute(&mut out, "key", &package.key)?;
                push_attribute(&mut out, "name", &package.name)?;
                out.push_str(">\r\n");

                if package.items.is_empty() {
                    out.push_str("   <items />\r\n");
                } else {
                    out.push_str("   <items>\r\n");
                    for item in &package.items {
                        out.push_str("    <item");
                        push_attribute(&mut out, "type", &item.item_type)?;
                        push_attribute(&mut out, "name", &item.name)?;
                        push_attribute(&mut out, "locator", &item.locator)?;
                        out.push_str(" />\r\n");
                    }
                    out.push_str("   </items>\r\n");
                }
                out.push_str("  </package>\r\n");
            }
            out.push_str(" </packages>\r\n");
        }

        out.push_str("</manifest>");
        Ok(out)
    }

    pub fn all_items(&self) -> impl Iterator<Item = &ItemRef> {
        self.packages
            .iter()
            .flat_map(|package| package.items.iter())
    }
}

fn self_start_element(
    name: &str,
    start: &BytesStart<'_>,
    stack: &[String],
    current_package: &mut Option<Package>,
    current_item: &mut Option<ItemRef>,
) {
    let depth = stack.len();
    if depth == 2 && name == "package" && stack.last().is_some_and(|n| n == "packages") {
        *current_package = Some(Package {
            package_type: attribute(start, "type").unwrap_or_default(),
            key: attribute(start, "key").unwrap_or_default(),
            name: attribute(start, "name").unwrap_or_default(),
            items: Vec::new(),
        });
    } else if depth == 4
        && name == "item"
        && stack.last().is_some_and(|n| n == "items")
        && stack.get(2).is_some_and(|n| n == "package")
    {
        *current_item = Some(ItemRef {
            item_type: attribute(start, "type").unwrap_or_default(),
            name: attribute(start, "name").unwrap_or_default(),
            locator: attribute(start, "locator").unwrap_or_default(),
        });
    }
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

fn push_attribute(out: &mut String, name: &str, value: &str) -> Result<()> {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    escape_attribute(out, value)?;
    out.push('"');
    Ok(())
}

fn escape_attribute(out: &mut String, value: &str) -> Result<()> {
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#x9;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            ch if (ch as u32) < 0x20 => {
                return Err(AppError::message(format!(
                    "manifest.xml: invalid XML character U+{:04X}",
                    ch as u32
                )));
            }
            ch if ch == '\u{FFFE}' || ch == '\u{FFFF}' => {
                return Err(AppError::message(format!(
                    "manifest.xml: invalid XML character U+{:04X}",
                    ch as u32
                )));
            }
            ch => out.push(ch),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = concat!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n",
        "<manifest version=\"2.0\">\r\n",
        " <packages>\r\n",
        "  <package type=\"part\" key=\"A&amp;B\" name=\"A\">\r\n",
        "   <items>\r\n",
        "    <item type=\"partxml\" name=\"part\" locator=\"A.part.xml\" />\r\n",
        "    <item type=\"macro\" name=\"graphicmacro\" locator=\"M\\ä.ema\" />\r\n",
        "   </items>\r\n",
        "  </package>\r\n",
        " </packages>\r\n",
        "</manifest>",
    );

    #[test]
    fn parse_and_serialize_round_trip() {
        let manifest = Manifest::parse(SAMPLE).unwrap();
        assert_eq!(manifest.version, "2.0");
        assert_eq!(manifest.packages.len(), 1);
        assert_eq!(manifest.packages[0].key, "A&B");
        assert_eq!(manifest.packages[0].items.len(), 2);
        assert_eq!(manifest.packages[0].items[1].locator, "M\\ä.ema");
        assert_eq!(manifest.to_xml().unwrap(), SAMPLE);
    }

    #[test]
    fn serializes_empty_packages_and_items() {
        let manifest = Manifest {
            version: "2.0".to_string(),
            packages: Vec::new(),
        };
        assert_eq!(
            manifest.to_xml().unwrap(),
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<manifest version=\"2.0\">\r\n <packages />\r\n</manifest>"
        );

        let manifest = Manifest {
            version: "2.0".to_string(),
            packages: vec![Package {
                package_type: "part".to_string(),
                key: "K".to_string(),
                name: "N".to_string(),
                items: Vec::new(),
            }],
        };
        assert_eq!(
            manifest.to_xml().unwrap(),
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<manifest version=\"2.0\">\r\n <packages>\r\n  <package type=\"part\" key=\"K\" name=\"N\">\r\n   <items />\r\n  </package>\r\n </packages>\r\n</manifest>"
        );
    }

    #[test]
    fn escapes_like_dotnet_xmlwriter() {
        let manifest = Manifest {
            version: "2.0".to_string(),
            packages: vec![Package {
                package_type: "part".to_string(),
                key: "A&B<C>D\"E'F".to_string(),
                name: "äöü€".to_string(),
                items: vec![ItemRef {
                    item_type: "x".to_string(),
                    name: String::new(),
                    locator: "a\"b&c<d>e'f".to_string(),
                }],
            }],
        };
        let xml = manifest.to_xml().unwrap();
        assert!(xml.contains("key=\"A&amp;B&lt;C&gt;D&quot;E'F\""));
        assert!(xml.contains("name=\"äöü€\""));
        assert!(xml.contains("locator=\"a&quot;b&amp;c&lt;d&gt;e'f\" />"));
    }
}
