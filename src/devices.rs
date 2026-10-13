//! Names of Apple devices the pods have hosted. The roster only carries addresses.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::types::Address;

pub fn load() -> HashMap<Address, String> {
    let mut labels = HashMap::new();
    if let Some(path) = fleet_path() {
        merge(&mut labels, &path, false);
    }
    // The previous install on this machine already learned these names.
    if let Some(path) = legacy_path() {
        merge(&mut labels, &path, true);
    }
    if let Some(path) = fleet_path() {
        let _ = save(&path, &labels);
    }
    labels
}

/// An Omarchy laptop announces itself on the pods as `Omarchy - <host>`.
/// That is a fleet peer name, never an Apple device name.
pub fn omarchy_label(label: &str) -> bool {
    label.trim().to_ascii_lowercase().starts_with("omarchy")
}

/// Family labels carry less identity than a model or a personal device name.
pub fn generic_label(label: &str) -> bool {
    matches!(
        label.trim().to_ascii_lowercase().as_str(),
        "mac" | "iphone" | "ipad" | "ipod" | "apple watch"
    )
}

pub fn valid_label(label: &str) -> Option<&str> {
    let label = label.trim();
    let address_parts: Vec<_> = label.split([':', '-']).collect();
    if label.is_empty()
        || label.len() > 248
        || label.chars().any(char::is_control)
        || Address::parse(label).is_some()
        || (address_parts.len() >= 3
            && address_parts
                .iter()
                .all(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_hexdigit())))
        || matches!(label.to_ascii_lowercase().as_str(), "unknown" | "apple device")
    {
        None
    } else {
        Some(label)
    }
}

pub fn store(labels: &HashMap<Address, String>) -> std::io::Result<()> {
    let path = fleet_path().ok_or_else(|| std::io::Error::other("no config directory"))?;
    save(&path, labels)
}

fn merge(labels: &mut HashMap<Address, String>, path: &Path, fill_gaps: bool) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let Ok(document) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    let Some(rows) = document.get("devices").and_then(|value| value.as_array()) else {
        return;
    };
    for row in rows {
        let kind = row.get("kind").and_then(|value| value.as_str()).unwrap_or("apple");
        if kind != "apple" {
            continue;
        }
        let Some(address) =
            row.get("address").and_then(|value| value.as_str()).and_then(Address::parse)
        else {
            continue;
        };
        let Some(label) = row.get("label").and_then(|value| value.as_str()) else {
            continue;
        };
        if omarchy_label(label) {
            continue;
        }
        let Some(label) = valid_label(label) else {
            continue;
        };
        if fill_gaps {
            labels
                .entry(address)
                .and_modify(|saved| {
                    if generic_label(saved) && !generic_label(label) {
                        *saved = label.to_string();
                    }
                })
                .or_insert_with(|| label.to_string());
        } else {
            labels.insert(address, label.to_string());
        }
    }
}

fn save(path: &Path, labels: &HashMap<Address, String>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut rows: Vec<_> = labels
        .iter()
        .map(|(address, label)| {
            serde_json::json!({
                "address": address.display(),
                "kind": "apple",
                "label": label,
            })
        })
        .collect();
    rows.sort_by(|left, right| left["address"].as_str().cmp(&right["address"].as_str()));
    let text = serde_json::to_string_pretty(&serde_json::json!({ "devices": rows }))
        .unwrap_or_else(|_| "{}".to_string());
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, text)?;
    fs::rename(temporary, path)
}

fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })
}

fn fleet_path() -> Option<PathBuf> {
    Some(config_home()?.join("fleetpods/devices.json"))
}

fn legacy_path() -> Option<PathBuf> {
    Some(config_home()?.join("omnipods/devices.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_names_upgrade_generic_labels_without_overwriting_specific_names() {
        let mac = Address::parse("02:00:00:00:00:01").unwrap();
        let phone = Address::parse("02:00:00:00:00:02").unwrap();
        let path =
            std::env::temp_dir().join(format!("fleetpods-name-import-{}.json", std::process::id()));
        save(
            &path,
            &HashMap::from([(mac, "MacBook Pro".into()), (phone, "Old iPhone name".into())]),
        )
        .unwrap();
        let mut labels = HashMap::from([(mac, "Mac".into()), (phone, "Work iPhone".into())]);
        merge(&mut labels, &path, true);
        assert_eq!(labels[&mac], "MacBook Pro");
        assert_eq!(labels[&phone], "Work iPhone");
        save(&path, &labels).unwrap();
        let mut reloaded = HashMap::new();
        merge(&mut reloaded, &path, false);
        assert_eq!(reloaded, labels);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn saved_names_survive_reload_and_renames_keep_other_devices() {
        let phone = Address::parse("28:02:2E:BF:7F:7D").unwrap();
        let mac = Address::parse("F0:2F:4B:14:DE:29").unwrap();
        let path =
            std::env::temp_dir().join(format!("fleetpods-name-save-{}.json", std::process::id()));
        let mut labels =
            HashMap::from([(phone, "Dan’s iPhone".into()), (mac, "MacBook Pro".into())]);
        save(&path, &labels).unwrap();
        let mut reloaded = HashMap::new();
        merge(&mut reloaded, &path, false);
        assert_eq!(reloaded, labels);
        labels.insert(mac, "Work: MacBook Pro".into());
        save(&path, &labels).unwrap();
        reloaded.clear();
        merge(&mut reloaded, &path, false);
        assert_eq!(reloaded, labels);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn an_apple_label_is_kept_and_an_address_is_not() {
        let text = r#"{"devices":[
            {"address":"28:02:2E:BF:7F:7D","kind":"apple","label":"iPhone"},
            {"address":"AA:BB:CC:DD:EE:FF","kind":"apple","label":"28:02:2E"},
            {"address":"D0:C6:37:2F:D0:9F","kind":"apple","label":"Omarchy - lg-gram"},
            {"address":"A8:72:4D:0C:7C:EE","kind":"omarchy","label":"omarchy-xps"}
        ]}"#;
        let mut labels = HashMap::new();
        let path =
            std::env::temp_dir().join(format!("fleetpods-names-{}.json", std::process::id()));
        fs::write(&path, text).unwrap();
        merge(&mut labels, &path, false);
        let _ = fs::remove_file(&path);
        let phone = Address::parse("28:02:2E:BF:7F:7D").unwrap();
        assert_eq!(labels.get(&phone).map(String::as_str), Some("iPhone"));
        assert!(!labels.contains_key(&Address::parse("D0:C6:37:2F:D0:9F").unwrap()));
        assert!(!labels.contains_key(&Address::parse("AA:BB:CC:DD:EE:FF").unwrap()));
        assert!(!labels.contains_key(&Address::parse("A8:72:4D:0C:7C:EE").unwrap()));
    }
}
