use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use crate::archive::{ArchiveIndex, ArchiveSource, lookup_key};
use crate::error::Result;
use crate::model::{ItemRef, Manifest, Package};
use crate::payload;
use crate::util::{base_name, file_stem};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosureMode {
    None,
    Related,
    Full,
}

impl ClosureMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "related" => Ok(Self::Related),
            "full" => Ok(Self::Full),
            other => Err(crate::error::AppError::usage(format!(
                "Unknown closure mode: {other}"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Related => "Related",
            Self::Full => "Full",
        }
    }
}

pub trait PayloadProvider {
    fn payload(&self, member_path: &str) -> Option<Arc<String>>;
}

/// Reads and caches payload XML by archive member path, case-insensitively.
pub struct ArchivePayloads<'a> {
    source: &'a dyn ArchiveSource,
    index: &'a ArchiveIndex,
    cache: Mutex<HashMap<String, Option<Arc<String>>>>,
}

impl<'a> ArchivePayloads<'a> {
    pub fn new(source: &'a dyn ArchiveSource, index: &'a ArchiveIndex) -> Self {
        Self {
            source,
            index,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn load(&self, key: &str) -> Option<Arc<String>> {
        let (member_index, _) = self.index.try_resolve(key)?;
        let bytes = self.source.read(self.index.member(member_index)).ok()?;
        Some(Arc::new(crate::util::decode_text(&bytes)))
    }
}

impl PayloadProvider for ArchivePayloads<'_> {
    fn payload(&self, member_path: &str) -> Option<Arc<String>> {
        let key = lookup_key(member_path);
        if let Ok(cache) = self.cache.lock()
            && let Some(cached) = cache.get(&key)
        {
            return cached.clone();
        }
        let value = self.load(&key);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(key, value.clone());
        }
        value
    }
}

pub struct DependencyGraph<'a> {
    model: &'a Manifest,
    index: &'a ArchiveIndex,
    packages_by_node: HashMap<String, usize>,
    packages_by_key: HashMap<String, Vec<usize>>,
    key_order: Vec<String>,
    item_owners: HashMap<String, Vec<(usize, usize)>>,
    item_order: Vec<String>,
    accessory_lists_by_part_key: HashMap<String, Vec<usize>>,
}

#[derive(Default)]
pub struct ClosureResult {
    pub packages: Vec<usize>,
    pub items: Vec<(usize, usize)>,
}

impl<'a> DependencyGraph<'a> {
    pub fn build(
        model: &'a Manifest,
        index: &'a ArchiveIndex,
        payloads: &dyn PayloadProvider,
    ) -> Self {
        let mut graph = Self {
            model,
            index,
            packages_by_node: HashMap::new(),
            packages_by_key: HashMap::new(),
            key_order: Vec::new(),
            item_owners: HashMap::new(),
            item_order: Vec::new(),
            accessory_lists_by_part_key: HashMap::new(),
        };

        for (package_index, package) in model.packages.iter().enumerate() {
            graph
                .packages_by_node
                .insert(package_node(package), package_index);
            if !package.key.is_empty() {
                let key_node = package_key_node(&package.package_type, &package.key);
                graph.packages_by_node.insert(key_node, package_index);

                let key = package.key.to_lowercase();
                if !graph.packages_by_key.contains_key(&key) {
                    graph.key_order.push(key.clone());
                }
                graph
                    .packages_by_key
                    .entry(key)
                    .or_default()
                    .push(package_index);
            }

            for (item_index, item) in package.items.iter().enumerate() {
                let key = lookup_key(&item.member_path());
                if !graph.item_owners.contains_key(&key) {
                    graph.item_order.push(key.clone());
                }
                graph
                    .item_owners
                    .entry(key)
                    .or_default()
                    .push((package_index, item_index));
            }
        }

        for (package_index, package) in model.packages.iter().enumerate() {
            if !package.package_type.eq_ignore_ascii_case("accessorylist") {
                continue;
            }
            for item in &package.items {
                if !item.name.eq_ignore_ascii_case("accessorylist") {
                    continue;
                }
                let Some(payload_text) = payloads.payload(&item.member_path()) else {
                    continue;
                };
                for (kind, value) in payload::extract_accessory_list_refs(&payload_text) {
                    if kind != "part" {
                        continue;
                    }
                    let Some(target) = graph.find_part(&value) else {
                        continue;
                    };
                    let key = graph.package(target).key.to_lowercase();
                    let lists = graph.accessory_lists_by_part_key.entry(key).or_default();
                    if !lists.contains(&package_index) {
                        lists.push(package_index);
                    }
                }
            }
        }

        graph
    }

    pub fn package(&self, index: usize) -> &'a Package {
        &self.model.packages[index]
    }

    pub fn resolve(&self, item: &ItemRef) -> Option<usize> {
        self.index
            .try_resolve(&item.member_path())
            .map(|(index, _)| index)
    }

    pub fn is_missing(&self, item: &ItemRef) -> bool {
        self.resolve(item).is_none()
    }

    pub fn accessory_lists_referencing(&self, part: &Package) -> &[usize] {
        if let Some(lists) = self
            .accessory_lists_by_part_key
            .get(&part.key.to_lowercase())
        {
            return lists;
        }
        let sanitized = crate::util::sanitize(&part.key.to_lowercase());
        self.accessory_lists_by_part_key
            .get(&sanitized)
            .map(|lists| lists.as_slice())
            .unwrap_or(&[])
    }

    pub fn expand_part_payload_refs(&self, payload_text: &str) -> Vec<String> {
        let mut nodes = Vec::new();
        for payload_ref in payload::extract_from_xml(payload_text) {
            if matches!(
                payload_ref.kind.as_str(),
                "construction" | "connectionpoints" | "accessoryId" | "isAccessory" | "external"
            ) {
                continue;
            }
            let Some(expanded) = payload::expand_md_variables(&payload_ref.raw_value) else {
                continue;
            };
            if self.index.try_resolve(&expanded).is_some() {
                nodes.push(item_node(&expanded));
            }
        }
        nodes
    }

    pub fn find_connection_points_items(&self, terminal_name: &str) -> Vec<String> {
        let mut nodes = Vec::new();
        let terminal = terminal_name.trim();
        if terminal.is_empty() {
            return nodes;
        }
        let terminal_lower = terminal.to_lowercase();

        for member_path in &self.item_order {
            let Some(owners) = self.item_owners.get(member_path) else {
                continue;
            };
            for (package_index, item_index) in owners {
                let owner_item = &self.model.packages[*package_index].items[*item_index];
                if !owner_item.name.eq_ignore_ascii_case("connectionpoints") {
                    continue;
                }
                let file_name = base_name(member_path);
                let mut stem = file_stem(&file_name);
                if stem.to_lowercase().ends_with(".connectionpoints") {
                    stem.truncate(stem.len() - ".connectionpoints".len());
                }
                if stem.to_lowercase().starts_with(&terminal_lower) {
                    nodes.push(item_node(member_path));
                }
            }
        }
        nodes
    }

    pub fn find_package(&self, package_type: &str, key: &str) -> Option<usize> {
        if package_type.trim().is_empty() || key.trim().is_empty() {
            return None;
        }
        let key = key.trim().to_lowercase();
        if let Some(list) = self.packages_by_key.get(&key)
            && let Some(index) = list.iter().copied().find(|index| {
                self.model.packages[*index]
                    .package_type
                    .eq_ignore_ascii_case(package_type)
            })
        {
            return Some(index);
        }
        let sanitized = crate::util::sanitize(&key);
        for candidate in &self.key_order {
            if crate::util::sanitize(candidate) != sanitized {
                continue;
            }
            if let Some(list) = self.packages_by_key.get(candidate)
                && let Some(index) = list.iter().copied().find(|index| {
                    self.model.packages[*index]
                        .package_type
                        .eq_ignore_ascii_case(package_type)
                })
            {
                return Some(index);
            }
        }
        None
    }

    pub fn find_part(&self, part_key: &str) -> Option<usize> {
        self.find_package("part", part_key)
    }

    pub fn find_construction(&self, construction_name: &str) -> Option<usize> {
        self.find_package("construction", construction_name)
    }

    pub fn compute_closure(
        &self,
        seed: usize,
        mode: ClosureMode,
        strict_part: bool,
        payloads: &dyn PayloadProvider,
    ) -> ClosureResult {
        let mut result = ClosureResult::default();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        let seed_node = package_node(&self.model.packages[seed]);
        enqueue(&mut visited, &mut queue, seed_node.clone());

        if mode != ClosureMode::None {
            for list_index in self.accessory_lists_referencing(&self.model.packages[seed]) {
                let list_index = *list_index;
                if strict_part && self.is_other_part(list_index, seed) {
                    continue;
                }
                enqueue(
                    &mut visited,
                    &mut queue,
                    package_node(&self.model.packages[list_index]),
                );
            }
        }

        while let Some(node) = queue.pop_front() {
            if let Some(member_key) = node.strip_prefix("item:") {
                let Some(owners) = self.item_owners.get(member_key) else {
                    continue;
                };

                for (owner_package, owner_item) in owners {
                    let package = &self.model.packages[*owner_package];
                    let item = &package.items[*owner_item];
                    if !item.name.eq_ignore_ascii_case("part")
                        || !item.item_type.eq_ignore_ascii_case("partxml")
                    {
                        continue;
                    }
                    let Some(payload_text) = payloads.payload(member_key) else {
                        continue;
                    };
                    if mode == ClosureMode::None {
                        continue;
                    }

                    for child in self.expand_part_payload_refs(&payload_text) {
                        enqueue(&mut visited, &mut queue, child);
                    }
                    for payload_ref in payload::extract_from_xml(&payload_text) {
                        if payload_ref.kind == "construction" {
                            if let Some(construction) =
                                self.find_construction(&payload_ref.raw_value)
                            {
                                enqueue(
                                    &mut visited,
                                    &mut queue,
                                    package_node(&self.model.packages[construction]),
                                );
                            }
                        } else if payload_ref.kind == "connectionpoints" {
                            for connection_point in
                                self.find_connection_points_items(&payload_ref.raw_value)
                            {
                                enqueue(&mut visited, &mut queue, connection_point);
                            }
                        }
                    }
                    if mode == ClosureMode::Full {
                        for list_index in self.accessory_lists_referencing(package) {
                            let list_index = *list_index;
                            if strict_part && self.is_other_part(list_index, seed) {
                                continue;
                            }
                            enqueue(
                                &mut visited,
                                &mut queue,
                                package_node(&self.model.packages[list_index]),
                            );
                        }
                    }
                }

                // Owner follow only for structural item kinds.
                if owners.iter().any(|(package_index, item_index)| {
                    is_structural_item(&self.model.packages[*package_index].items[*item_index])
                }) {
                    for (owner_package, _) in owners {
                        if strict_part && self.is_other_part(*owner_package, seed) {
                            continue;
                        }
                        enqueue(
                            &mut visited,
                            &mut queue,
                            package_node(&self.model.packages[*owner_package]),
                        );
                    }
                }
                continue;
            }

            let Some(mut package_index) = self.packages_by_node.get(&node).copied() else {
                continue;
            };
            if strict_part && self.is_other_part(package_index, seed) {
                if node == seed_node {
                    package_index = seed;
                } else {
                    continue;
                }
            }

            let package = &self.model.packages[package_index];
            result.packages.push(package_index);
            for (item_index, item) in package.items.iter().enumerate() {
                result.items.push((package_index, item_index));
                enqueue(&mut visited, &mut queue, item_node(&item.member_path()));
            }

            if mode == ClosureMode::Full
                && package.package_type.eq_ignore_ascii_case("accessorylist")
            {
                for item in &package.items {
                    if !item.name.eq_ignore_ascii_case("accessorylist") {
                        continue;
                    }
                    let Some(payload_text) = payloads.payload(&item.member_path()) else {
                        continue;
                    };
                    for (kind, value) in payload::extract_accessory_list_refs(&payload_text) {
                        if kind != "part" {
                            continue;
                        }
                        let Some(target) = self.find_part(&value) else {
                            continue;
                        };
                        if strict_part && self.is_other_part(target, seed) {
                            continue;
                        }
                        enqueue(
                            &mut visited,
                            &mut queue,
                            package_node(&self.model.packages[target]),
                        );
                    }
                }
            }
        }

        result
    }

    fn is_other_part(&self, package_index: usize, seed: usize) -> bool {
        package_index != seed
            && self.model.packages[package_index]
                .package_type
                .eq_ignore_ascii_case("part")
    }
}

fn enqueue(visited: &mut HashSet<String>, queue: &mut VecDeque<String>, node: String) {
    if visited.insert(node.clone()) {
        queue.push_back(node);
    }
}

pub fn package_node(package: &Package) -> String {
    package_key_node(&package.package_type, &package.key)
}

pub fn package_key_node(package_type: &str, key: &str) -> String {
    format!("pkg:{package_type}:{key}").to_lowercase()
}

pub fn item_node(member_path: &str) -> String {
    format!("item:{}", lookup_key(member_path))
}

fn is_structural_item(item: &ItemRef) -> bool {
    item.name.eq_ignore_ascii_case("connectionpoints")
        || item.name.eq_ignore_ascii_case("accessorylist")
        || item.name.eq_ignore_ascii_case("accessoryplacement")
        || item.name.eq_ignore_ascii_case("construction")
        || item.name.eq_ignore_ascii_case("contour")
}
