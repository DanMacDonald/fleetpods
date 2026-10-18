//! BlueZ over the system bus. One paired accessory, found by address or by
//! the control service, on whichever adapter owns it.
//!
//! This module does not complete the accessory handshake. The packet layout
//! lives in the GPL daemon and is not copied here.

use std::collections::HashMap;
use std::time::Duration;

use dbus::arg::{RefArg, Variant};
use dbus::blocking::stdintf::org_freedesktop_dbus::ObjectManager;
use dbus::blocking::{Connection, Proxy};
use dbus::message::{Message, MessageType};

use crate::types::Address;

/// A bus connection whose file descriptor can be polled. `watch` panics until this is set.
pub(crate) fn open_bus(bus: dbus::channel::BusType) -> Result<Connection, String> {
    let mut channel = dbus::channel::Channel::get_private(bus).map_err(|err| err.to_string())?;
    channel.set_watch_enabled(true);
    Ok(Connection::from(channel))
}

/// Apple's AAP control service. A paired device that advertises it is the accessory.
pub const CONTROL_SERVICE: &str = "74ec2172-0bad-4d01-8f77-997b2be0722a";

/// A2DP sink transport. The codec is a card profile, not a different UUID.
pub const A2DP_SINK: &str = "0000110b-0000-1000-8000-00805f9b34fb";

/// L2CAP PSM for that service.
pub const CONTROL_PSM: u16 = 0x1001;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub path: String,
    pub adapter_path: String,
    pub adapter: Option<Address>,
    pub address: Address,
    pub name: String,
    pub rssi: Option<i16>,
    pub paired: bool,
    pub bonded: bool,
    pub connected: bool,
    pub control: bool,
}

impl Device {
    /// A stored Bluetooth bond remains usable between connections. Trust alone
    /// is permission to connect, not evidence of exchanged pairing keys.
    pub fn has_pairing(&self) -> bool {
        self.paired || self.bonded
    }
}

/// The configured address wins. Otherwise the only paired AirPods, or the
/// only one of those that is already connected.
pub fn select(devices: &[Device], configured: Option<Address>) -> Option<&Device> {
    if let Some(want) = configured {
        return devices.iter().find(|device| device.has_pairing() && device.address == want);
    }
    let mut pods: Vec<_> =
        devices.iter().filter(|device| device.has_pairing() && looks_like_pods(device)).collect();
    if pods.len() == 1 {
        return Some(pods[0]);
    }
    pods.retain(|device| device.connected);
    if pods.len() == 1 {
        Some(pods[0])
    } else {
        None
    }
}

pub fn looks_like_pods(device: &Device) -> bool {
    device.control || device.name.to_ascii_lowercase().contains("airpods")
}

/// `AA:BB:CC:DD:EE:FF` stored in the order the L2CAP address expects.
pub fn bdaddr(address: Address) -> [u8; 6] {
    let bytes = address.bytes();
    [bytes[5], bytes[4], bytes[3], bytes[2], bytes[1], bytes[0]]
}

pub struct Bus {
    connection: Connection,
}

pub struct Inventory {
    pub adapters: HashMap<String, Address>,
    pub devices: Vec<Device>,
}

impl Bus {
    pub fn system() -> Result<Self, String> {
        open_bus(dbus::channel::BusType::System).map(|connection| Self { connection })
    }

    pub fn inventory(&self) -> Result<Inventory, String> {
        let proxy = self.connection.with_proxy("org.bluez", "/", Duration::from_secs(2));
        let objects = proxy.get_managed_objects().map_err(|err| err.to_string())?;
        Ok(Inventory { adapters: adapters_from(&objects), devices: devices_from(&objects) })
    }

    pub fn connect(&self, path: &str) -> Result<ConnectResult, String> {
        let proxy = self.connection.with_proxy("org.bluez", path, Duration::from_secs(25));
        match proxy.method_call::<(), _, _, _>("org.bluez.Device1", "Connect", ()) {
            Ok(()) => Ok(ConnectResult::Done),
            Err(err) => {
                let name = err.name().unwrap_or("");
                if name.ends_with("AlreadyConnected") {
                    Ok(ConnectResult::Done)
                } else if name.ends_with("InProgress") {
                    Ok(ConnectResult::InProgress)
                } else {
                    Err(err.to_string())
                }
            }
        }
    }

    pub fn connected(&self, path: &str) -> Result<bool, String> {
        let proxy: Proxy<&Connection> =
            self.connection.with_proxy("org.bluez", path, Duration::from_secs(2));
        let (value,): (Variant<bool>,) = proxy
            .method_call(
                "org.freedesktop.DBus.Properties",
                "Get",
                ("org.bluez.Device1", "Connected"),
            )
            .map_err(|err| err.to_string())?;
        Ok(value.0)
    }

    /// Allow BlueZ to save a bond during user-initiated pairing. This neither
    /// makes the adapter discoverable nor calls Device1.Pair.
    pub fn enable_pairing(&self, adapter: &str) -> Result<(), String> {
        use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
        let proxy = self.connection.with_proxy("org.bluez", adapter, Duration::from_secs(2));
        let pairable: bool =
            proxy.get("org.bluez.Adapter1", "Pairable").map_err(|err| err.to_string())?;
        if !pairable {
            proxy.set("org.bluez.Adapter1", "Pairable", true).map_err(|err| err.to_string())?;
            eprintln!("fleetpodsd: enabled Bluetooth pairing so setup can save a bond");
        }
        Ok(())
    }

    /// Whether the adapter reports any discovery session running right now.
    pub fn discovering(&self, adapter: &str) -> Result<bool, String> {
        use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
        let proxy = self.connection.with_proxy("org.bluez", adapter, Duration::from_secs(2));
        proxy.get("org.bluez.Adapter1", "Discovering").map_err(|err| err.to_string())
    }

    pub fn disconnect(&self, path: &str) -> Result<(), String> {
        let proxy = self.connection.with_proxy("org.bluez", path, Duration::from_secs(8));
        call_device(proxy, "Disconnect", ())
    }

    pub fn connect_audio(&self, path: &str) -> Result<(), String> {
        let proxy = self.connection.with_proxy("org.bluez", path, Duration::from_secs(25));
        call_device(proxy, "ConnectProfile", (A2DP_SINK,))
    }

    /// The unique bus name that currently owns a well-known name, if any.
    pub fn name_owner(&self, name: &str) -> Option<String> {
        let proxy = self.connection.with_proxy(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            Duration::from_secs(2),
        );
        let (owner,): (String,) =
            proxy.method_call("org.freedesktop.DBus", "GetNameOwner", (name,)).ok()?;
        Some(owner)
    }

    pub fn fd(&self) -> std::os::fd::RawFd {
        self.connection.channel().watch().fd
    }

    /// Ask BlueZ to send signals. One blocking round trip, then the poll loop takes over.
    pub fn listen(&self) -> Result<(), String> {
        self.connection
            .add_match_no_cb("type='signal',sender='org.bluez'")
            .map_err(|err| err.to_string())?;
        self.connection
            .add_match_no_cb(
                "type='signal',interface='org.freedesktop.DBus',member='NameOwnerChanged'",
            )
            .map_err(|err| err.to_string())?;
        // logind may be absent. Bluetooth signals still matter.
        let _ = self.connection.add_match_no_cb(
            "type='signal',sender='org.freedesktop.login1',interface='org.freedesktop.login1.Manager',member='PrepareForSleep'",
        );
        Ok(())
    }

    /// Read one queued message. Call only when the bus fd is readable.
    pub fn pull(&self) -> Result<(), String> {
        self.connection
            .channel()
            .read_write(Some(Duration::ZERO))
            .map_err(|_| "bluez read failed".to_string())
    }

    pub fn pop(&self) -> Option<Message> {
        self.connection.channel().pop_message()
    }

    pub fn submit(&self, message: Message) -> Result<u32, String> {
        let serial = self
            .connection
            .channel()
            .send(message)
            .map_err(|_| "bluetooth send failed".to_string())?;
        self.connection.channel().flush();
        Ok(serial)
    }

    /// Ask the adapter to drop its pairing for this device path.
    pub fn remove_device(&self, adapter: &str, device: &str) -> Result<u32, String> {
        let path = dbus::Path::new(device).map_err(|err| err.to_string())?;
        let message =
            Message::new_method_call("org.bluez", adapter, "org.bluez.Adapter1", "RemoveDevice")?
                .append1(path);
        self.submit(message)
    }

    /// Queue a Device1 call and flush it. The reply arrives later, matched by serial.
    pub fn call(&self, path: &str, method: &str, arg: Option<&str>) -> Result<u32, String> {
        let mut message = Message::new_method_call("org.bluez", path, "org.bluez.Device1", method)?;
        if let Some(arg) = arg {
            message = message.append1(arg);
        }
        let serial =
            self.connection.channel().send(message).map_err(|_| "bluez send failed".to_string())?;
        self.connection.channel().flush();
        Ok(serial)
    }
}

pub enum Notice {
    Reply {
        serial: u32,
        result: CallResult,
    },
    Removed(String),
    Upsert {
        device: Device,
        fleet: Option<Vec<u8>>,
        apple: Option<Vec<u8>>,
    },
    Adapter {
        path: String,
        address: Address,
    },
    /// `present` is false when bluetoothd left the bus.
    Name {
        present: bool,
    },
    /// logind `PrepareForSleep`. `true` means the machine is about to sleep.
    Sleep(bool),
    /// Adapter1 `Powered`.
    AdapterPower {
        path: String,
        up: bool,
    },
    DiscoveryStopped(String),
    Patch(Patch),
}

pub enum CallResult {
    Done,
    Failed { name: String, message: String },
}

pub struct Patch {
    pub path: String,
    pub connected: Option<bool>,
    pub paired: Option<bool>,
    pub bonded: Option<bool>,
    pub name: Option<String>,
    pub rssi: Option<i16>,
    pub uuids: Option<Vec<String>>,
    /// Fleet service data, when this change included that UUID.
    pub fleet: Option<Vec<u8>>,
    /// Apple manufacturer payload, when this change included company id `0x004C`.
    pub apple: Option<Vec<u8>>,
}

/// May this sender's signal be believed? Broadcast match rules constrain
/// `sender=`, but a unicast signal addressed straight to this daemon is
/// delivered regardless of them, so the sender must be the unique name
/// that owns the well-known service for the signal's interface: the bus
/// itself for `NameOwnerChanged`, logind for `PrepareForSleep`, and BlueZ
/// for everything else (device, adapter, and object-manager signals).
pub fn sender_allowed(
    sender: Option<&str>,
    interface: &str,
    bluez_owner: Option<&str>,
    login1_owner: Option<&str>,
) -> bool {
    let Some(sender) = sender else {
        return false;
    };
    match interface {
        "org.freedesktop.DBus" => sender == "org.freedesktop.DBus",
        "org.freedesktop.login1.Manager" => login1_owner.is_some_and(|owner| sender == owner),
        _ => bluez_owner.is_some_and(|owner| sender == owner),
    }
}

pub fn interpret(mut message: Message) -> Option<Notice> {
    if let Some(serial) = message.get_reply_serial() {
        return Some(Notice::Reply { serial, result: call_result(&mut message) });
    }
    if message.msg_type() != MessageType::Signal {
        return None;
    }
    let interface = message.interface().map(|value| value.to_string()).unwrap_or_default();
    let member = message.member().map(|value| value.to_string()).unwrap_or_default();
    match (interface.as_str(), member.as_str()) {
        ("org.freedesktop.DBus", "NameOwnerChanged") => name_owner(&message),
        ("org.freedesktop.login1.Manager", "PrepareForSleep") => {
            message.read1().ok().map(Notice::Sleep)
        }
        ("org.freedesktop.DBus.Properties", "PropertiesChanged") => properties(&message),
        ("org.freedesktop.DBus.ObjectManager", "InterfacesAdded") => added(&message),
        ("org.freedesktop.DBus.ObjectManager", "InterfacesRemoved") => removed(&message),
        _ => None,
    }
}

pub fn device_from(
    path: &str,
    props: &Props,
    adapters: &HashMap<String, Address>,
) -> Option<Device> {
    let address = prop_string(props, "Address").and_then(|text| Address::parse(&text))?;
    let adapter_path = prop_text(props, "Adapter").unwrap_or_default();
    let uuids = prop_strings(props, "UUIDs");
    Some(Device {
        path: path.to_string(),
        adapter_path: adapter_path.clone(),
        adapter: adapters.get(&adapter_path).copied(),
        address,
        name: prop_string(props, "Name").unwrap_or_default(),
        rssi: prop_rssi(props),
        paired: prop_bool(props, "Paired"),
        bonded: prop_bool(props, "Bonded"),
        connected: prop_bool(props, "Connected"),
        control: uuids.iter().any(|uuid| uuid.eq_ignore_ascii_case(CONTROL_SERVICE)),
    })
}

fn call_result(message: &mut Message) -> CallResult {
    match message.as_result() {
        Ok(_) => CallResult::Done,
        Err(err) => CallResult::Failed {
            name: err.name().unwrap_or("").to_string(),
            message: err.message().unwrap_or("").to_string(),
        },
    }
}

fn name_owner(message: &Message) -> Option<Notice> {
    let (name, _old, new): (String, String, String) = message.read3().ok()?;
    if name != "org.bluez" {
        return None;
    }
    Some(Notice::Name { present: !new.is_empty() })
}

fn properties(message: &Message) -> Option<Notice> {
    let path = message.path()?.to_string();
    let (interface, changed, _invalid): (String, Props, Vec<String>) = message.read3().ok()?;
    if interface == "org.bluez.MediaTransport1" {
        trace_transport(&path, &changed, "event");
        return None;
    }
    if interface == "org.bluez.Adapter1" {
        if let Some(address) =
            prop_string(&changed, "Address").and_then(|text| Address::parse(&text))
        {
            return Some(Notice::Adapter { path, address });
        }
        if let Some(up) = flag(&changed, "Powered") {
            return Some(Notice::AdapterPower { path, up });
        }
        if flag(&changed, "Discovering") == Some(false) {
            return Some(Notice::DiscoveryStopped(path));
        }
        return None;
    }
    if interface != "org.bluez.Device1" {
        return None;
    }
    Some(Notice::Patch(Patch {
        path,
        connected: flag(&changed, "Connected"),
        paired: flag(&changed, "Paired"),
        bonded: flag(&changed, "Bonded"),
        name: prop_string(&changed, "Name"),
        rssi: prop_rssi(&changed),
        uuids: changed.get("UUIDs").map(|_| prop_strings(&changed, "UUIDs")),
        fleet: fleet_service_data(changed.get("ServiceData").map(|value| value.0.as_ref())),
        apple: apple_payload(&changed),
    }))
}

pub fn address_of_path(path: &str) -> Option<Address> {
    let name = path.rsplit('/').next()?;
    let hex = name.strip_prefix("dev_")?;
    Address::parse(&hex.replace('_', ":"))
}

fn apple_payload(props: &Props) -> Option<Vec<u8>> {
    let value = props.get("ManufacturerData")?;
    company_payload(value.0.as_ref(), 0x004C)
}

fn company_payload(value: &dyn RefArg, company: u64) -> Option<Vec<u8>> {
    let mut items = value.as_iter()?;
    while let Some(id) = items.next() {
        let Some(data) = items.next() else {
            break;
        };
        if id.as_u64() == Some(company) {
            return Some(byte_list(data));
        }
    }
    None
}

fn byte_list(value: &dyn RefArg) -> Vec<u8> {
    let mut bytes = Vec::new();
    collect_bytes(value, &mut bytes);
    bytes
}

fn collect_bytes(value: &dyn RefArg, out: &mut Vec<u8>) {
    if value.as_iter().is_none() {
        if let Some(byte) = value.as_u64().filter(|n| *n <= u64::from(u8::MAX)) {
            out.push(byte as u8);
        }
        return;
    }
    if let Some(items) = value.as_iter() {
        for item in items {
            collect_bytes(item, out);
        }
    }
}

fn fleet_service_data(value: Option<&dyn RefArg>) -> Option<Vec<u8>> {
    let mut items = value?.as_iter()?;
    let uuid = crate::fleet::wire::FLEET_SERVICE_UUID;
    while let Some(key) = items.next() {
        let Some(data) = items.next() else {
            break;
        };
        if key.as_str().is_some_and(|key| key.eq_ignore_ascii_case(uuid)) {
            return Some(byte_list(data));
        }
    }
    None
}

fn added(message: &Message) -> Option<Notice> {
    let (path, interfaces): (dbus::Path, HashMap<String, Props>) = message.read2().ok()?;
    if let Some(props) = interfaces.get("org.bluez.MediaTransport1") {
        trace_transport(&path, props, "snapshot");
    }
    if let Some(props) = interfaces.get("org.bluez.Adapter1") {
        if let Some(address) = prop_string(props, "Address").and_then(|text| Address::parse(&text))
        {
            return Some(Notice::Adapter { path: path.to_string(), address });
        }
    }
    let props = interfaces.get("org.bluez.Device1")?;
    Some(Notice::Upsert {
        device: device_from(&path.to_string(), props, &HashMap::new())?,
        fleet: fleet_service_data(props.get("ServiceData").map(|value| value.0.as_ref())),
        apple: apple_payload(props),
    })
}

fn trace_transport(path: &str, props: &Props, kind: &str) {
    // These are reported values, not evidence of a physical button press.
    // Only log known media properties; never dump the complete property map.
    if let Some(volume) = props.get("Volume").and_then(|value| value.0.as_u64()) {
        eprintln!(
            "fleetpodsd: media {kind} source=bluez transport={path:?} volume={volume} scale=0..127"
        );
    }
    if let Some(state) = prop_string(props, "State") {
        eprintln!("fleetpodsd: media {kind} source=bluez transport={path:?} state={state:?}");
    }
}

fn removed(message: &Message) -> Option<Notice> {
    let (path, interfaces): (dbus::Path, Vec<String>) = message.read2().ok()?;
    if interfaces.iter().any(|interface| interface == "org.bluez.Device1") {
        Some(Notice::Removed(path.to_string()))
    } else {
        None
    }
}

fn flag(props: &Props, key: &str) -> Option<bool> {
    props.get(key).map(|_| prop_bool(props, key))
}

fn call_device<A: dbus::arg::AppendAll>(
    proxy: Proxy<&Connection>,
    method: &str,
    args: A,
) -> Result<(), String> {
    match proxy.method_call::<(), _, _, _>("org.bluez.Device1", method, args) {
        Ok(()) => Ok(()),
        Err(err) => {
            let name = err.name().unwrap_or("");
            if name.ends_with("InProgress")
                || name.ends_with("AlreadyConnected")
                || name.ends_with("NotConnected")
            {
                Ok(())
            } else {
                Err(err.to_string())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectResult {
    Done,
    InProgress,
}

type Props = HashMap<String, Variant<Box<dyn RefArg>>>;
type Interfaces = HashMap<String, Props>;
type Objects = HashMap<dbus::Path<'static>, Interfaces>;

fn adapters_from(objects: &Objects) -> HashMap<String, Address> {
    let mut adapters = HashMap::new();
    for (path, interfaces) in objects {
        let Some(props) = interfaces.get("org.bluez.Adapter1") else {
            continue;
        };
        if let Some(address) = prop_string(props, "Address").and_then(|text| Address::parse(&text))
        {
            adapters.insert(path.to_string(), address);
        }
    }
    adapters
}

fn devices_from(objects: &Objects) -> Vec<Device> {
    let adapters = adapters_from(objects);
    let mut devices = Vec::new();
    for (path, interfaces) in objects {
        let Some(props) = interfaces.get("org.bluez.Device1") else {
            continue;
        };
        let Some(address) = prop_string(props, "Address").and_then(|text| Address::parse(&text))
        else {
            continue;
        };
        let adapter_path = prop_text(props, "Adapter").unwrap_or_default();
        let uuids = prop_strings(props, "UUIDs");
        devices.push(Device {
            path: path.to_string(),
            adapter: adapters.get(&adapter_path).copied(),
            adapter_path,
            address,
            name: prop_string(props, "Name").unwrap_or_default(),
            rssi: prop_rssi(props),
            paired: prop_bool(props, "Paired"),
            bonded: prop_bool(props, "Bonded"),
            connected: prop_bool(props, "Connected"),
            control: uuids.iter().any(|uuid| uuid.eq_ignore_ascii_case(CONTROL_SERVICE)),
        });
    }
    devices
}

fn prop_rssi(props: &Props) -> Option<i16> {
    i16::try_from(props.get("RSSI")?.0.as_i64()?).ok()
}

fn prop_string(props: &Props, key: &str) -> Option<String> {
    prop_text(props, key)
}

/// BlueZ strings are strings. The adapter property is an object path.
fn prop_text(props: &Props, key: &str) -> Option<String> {
    let value = props.get(key)?;
    if let Some(text) = value.0.as_str() {
        return Some(text.to_string());
    }
    value.0.as_any().downcast_ref::<dbus::Path<'static>>().map(|path| (**path).to_string())
}

fn prop_bool(props: &Props, key: &str) -> bool {
    props.get(key).and_then(|value| value.0.as_i64()).unwrap_or(0) != 0
}

fn prop_strings(props: &Props, key: &str) -> Vec<String> {
    let Some(value) = props.get(key) else {
        return Vec::new();
    };
    let Some(items) = value.0.as_iter() else {
        return Vec::new();
    };
    items.filter_map(|item| item.as_str().map(str::to_string)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_is_available_without_any_device_objects() {
        let address = Address::parse("02:00:00:00:00:01").unwrap();
        let mut props: Props = HashMap::new();
        props.insert("Address".into(), Variant(Box::new(address.display())));
        let objects = HashMap::from([(
            dbus::Path::from("/org/bluez/hci0"),
            HashMap::from([("org.bluez.Adapter1".into(), props)]),
        )]);
        assert!(devices_from(&objects).is_empty());
        assert_eq!(adapters_from(&objects).get("/org/bluez/hci0"), Some(&address));
    }

    fn device(mark: u8, name: &str, paired: bool, connected: bool, control: bool) -> Device {
        Device {
            path: format!("/org/bluez/hci0/dev_{mark}"),
            adapter_path: "/org/bluez/hci1".into(),
            adapter: Address::parse("02:00:00:00:00:01"),
            address: Address::new([mark, 0, 0, 0, 0, 1]).unwrap(),
            name: name.into(),
            rssi: None,
            paired,
            bonded: false,
            connected,
            control,
        }
    }

    #[test]
    fn an_adapter_object_path_is_read() {
        let mut props = HashMap::new();
        props.insert(
            "Adapter".into(),
            Variant(Box::new(dbus::Path::from("/org/bluez/hci0")) as Box<dyn RefArg>),
        );
        assert_eq!(prop_text(&props, "Adapter").as_deref(), Some("/org/bluez/hci0"));
    }

    #[test]
    fn one_paired_airpods_is_chosen_without_an_adapter_index() {
        let mouse = device(1, "Mouse", true, true, false);
        let pods = device(2, "David's AirPods Pro", true, false, false);
        let other = device(3, "Someone's AirPods", false, false, true);
        let devices = [mouse, pods.clone(), other];
        let chosen = select(&devices, None).unwrap();
        assert_eq!(chosen.address, pods.address);
        assert_eq!(chosen.adapter.unwrap().display(), "02:00:00:00:00:01");
    }

    #[test]
    fn saved_bond_allows_reconnect_but_trust_alone_does_not() {
        let path = "/org/bluez/hci0/dev_40_11_22_83_AC_CD";
        let mut props = Props::new();
        props.insert("Address".into(), Variant(Box::new("40:11:22:83:AC:CD".to_string())));
        props.insert("Name".into(), Variant(Box::new("AirPods".to_string())));
        props.insert("Trusted".into(), Variant(Box::new(true)));
        props.insert("Paired".into(), Variant(Box::new(false)));
        for bonded in [false, true] {
            props.insert("Bonded".into(), Variant(Box::new(bonded)));
            let objects = HashMap::from([(
                dbus::Path::from(path),
                HashMap::from([(
                    "org.bluez.Device1".into(),
                    props
                        .iter()
                        .map(|(key, value)| (key.clone(), Variant(value.0.box_clone())))
                        .collect(),
                )]),
            )]);
            let added = device_from(path, &props, &HashMap::new()).unwrap();
            let inventory = devices_from(&objects);
            assert_eq!(inventory, vec![added.clone()]);
            for configured in [None, Some(added.address)] {
                assert_eq!(select(&inventory, configured).is_some(), bonded);
            }
        }
        let changed = HashMap::from([("Bonded".to_string(), Variant(true))]);
        let message =
            Message::new_signal(path, "org.freedesktop.DBus.Properties", "PropertiesChanged")
                .unwrap()
                .append3("org.bluez.Device1", changed, Vec::<String>::new());
        let Some(Notice::Patch(patch)) = interpret(message) else {
            panic!("expected bond change");
        };
        assert_eq!(patch.bonded, Some(true));
        assert_eq!(patch.paired, None);
    }

    #[test]
    fn only_the_owning_service_may_send_its_signals() {
        let bluez = Some(":1.7");
        let login1 = Some(":1.3");
        // Genuine senders pass, including the bus itself.
        assert!(sender_allowed(Some(":1.7"), "org.freedesktop.DBus.Properties", bluez, login1));
        assert!(sender_allowed(Some(":1.3"), "org.freedesktop.login1.Manager", bluez, login1));
        assert!(sender_allowed(
            Some("org.freedesktop.DBus"),
            "org.freedesktop.DBus",
            bluez,
            login1
        ));
        // Another local user's unique name can forge the interface and
        // member of a unicast signal, but not the owner comparison.
        assert!(!sender_allowed(Some(":1.99"), "org.freedesktop.login1.Manager", bluez, login1));
        assert!(!sender_allowed(Some(":1.99"), "org.freedesktop.DBus.Properties", bluez, login1));
        assert!(!sender_allowed(Some(":1.7"), "org.freedesktop.login1.Manager", bluez, login1));
        assert!(!sender_allowed(None, "org.freedesktop.DBus.Properties", bluez, login1));
        // With no owner known, nothing is believed.
        assert!(!sender_allowed(Some(":1.7"), "org.freedesktop.DBus.Properties", None, login1));
    }

    #[test]
    fn a_configured_address_beats_another_paired_set() {
        let first = device(2, "AirPods", true, true, true);
        let second = device(4, "AirPods Pro", true, false, true);
        let want = second.address;
        let devices = [first, second];
        let chosen = select(&devices, Some(want)).unwrap();
        assert_eq!(chosen.address, want);
    }

    #[test]
    fn two_paired_sets_are_not_a_guess_unless_one_is_connected() {
        let first = device(2, "AirPods", true, false, true);
        let second = device(4, "AirPods Pro", true, true, true);
        assert!(
            select(&[first.clone(), device(4, "AirPods Pro", true, false, true)], None).is_none()
        );
        assert_eq!(select(&[first, second.clone()], None).unwrap().address, second.address);
    }

    #[test]
    fn the_l2cap_address_reverses_the_display_order() {
        let address = Address::parse("11:22:33:44:55:66").unwrap();
        assert_eq!(bdaddr(address), [0x66, 0x55, 0x44, 0x33, 0x22, 0x11]);
    }

    #[test]
    fn discovery_metadata_preserves_name_and_signed_optional_rssi() {
        let mut props = Props::new();
        props.insert("Address".into(), Variant(Box::new("40:11:22:83:AC:CD".to_string())));
        props.insert("Name".into(), Variant(Box::new("My AirPods".to_string())));
        props.insert("RSSI".into(), Variant(Box::new(-67_i16)));
        let device =
            device_from("/org/bluez/hci0/dev_40_11_22_83_AC_CD", &props, &HashMap::new()).unwrap();
        assert_eq!(device.name, "My AirPods");
        assert_eq!(device.rssi, Some(-67));
        props.remove("RSSI");
        assert_eq!(prop_rssi(&props), None);
    }

    #[test]
    fn a_live_manufacturer_change_carries_the_apple_payload() {
        use dbus::arg::Variant;
        use dbus::message::Message;
        use std::collections::HashMap;

        let payload = vec![0x07_u8, 0x19, 0x01];
        let mut manufacturers = HashMap::new();
        manufacturers.insert(0x004C_u16, Variant(payload.clone()));
        let mut changed = HashMap::new();
        changed.insert("ManufacturerData".to_string(), Variant(manufacturers));
        let message = Message::new_signal(
            "/org/bluez/hci0/dev_40_11_22_83_AC_CD",
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
        )
        .unwrap()
        .append3("org.bluez.Device1", changed, Vec::<String>::new());
        match interpret(message) {
            Some(Notice::Patch(patch)) => {
                assert_eq!(patch.apple.as_deref(), Some(payload.as_slice()));
                assert_eq!(address_of_path(&patch.path).unwrap().display(), "40:11:22:83:AC:CD");
                assert!(patch.fleet.is_none());
            }
            _ => panic!("expected a manufacturer patch"),
        }
    }

    #[test]
    fn a_live_service_change_carries_the_fleet_payload() {
        use dbus::arg::Variant;
        use dbus::message::Message;
        use std::collections::HashMap;

        let payload = vec![0x04_u8, 0x01, 0x02];
        let mut services = HashMap::new();
        services
            .insert(crate::fleet::wire::FLEET_SERVICE_UUID.to_string(), Variant(payload.clone()));
        let mut changed = HashMap::new();
        changed.insert("ServiceData".to_string(), Variant(services));
        let message = Message::new_signal(
            "/org/bluez/hci0/dev_40_11_22_83_AC_CD",
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
        )
        .unwrap()
        .append3("org.bluez.Device1", changed, Vec::<String>::new());
        match interpret(message) {
            Some(Notice::Patch(patch)) => {
                assert_eq!(patch.fleet.as_deref(), Some(payload.as_slice()));
                assert!(patch.apple.is_none());
            }
            _ => panic!("expected a service-data patch"),
        }
    }

    #[test]
    fn a_new_device_carries_its_first_fleet_and_apple_advertisement() {
        let fleet = vec![0x05_u8, 0x01, 0x02];
        let apple = vec![0x07_u8, 0x19, 0x01];
        let mut services = HashMap::new();
        services.insert(crate::fleet::wire::FLEET_SERVICE_UUID.to_string(), Variant(fleet.clone()));
        let mut manufacturers = HashMap::new();
        manufacturers.insert(0x004C_u16, Variant(apple.clone()));
        let mut props: Props = HashMap::new();
        props.insert("Address".into(), Variant(Box::new("40:11:22:83:AC:CD".to_string())));
        props.insert("ServiceData".into(), Variant(Box::new(services)));
        props.insert("ManufacturerData".into(), Variant(Box::new(manufacturers)));
        let interfaces = HashMap::from([("org.bluez.Device1".to_string(), props)]);
        let path = dbus::Path::new("/org/bluez/hci0/dev_40_11_22_83_AC_CD").unwrap();
        let message =
            Message::new_signal("/", "org.freedesktop.DBus.ObjectManager", "InterfacesAdded")
                .unwrap()
                .append2(path, interfaces);
        match interpret(message) {
            Some(Notice::Upsert { device, fleet: heard_fleet, apple: heard_apple }) => {
                assert_eq!(device.address.display(), "40:11:22:83:AC:CD");
                assert_eq!(heard_fleet, Some(fleet));
                assert_eq!(heard_apple, Some(apple));
            }
            _ => panic!("expected the first advertisement with the device"),
        }
    }

    #[test]
    fn sleep_and_adapter_power_are_their_own_notices() {
        use dbus::arg::Variant;
        use dbus::message::Message;
        use std::collections::HashMap;

        let sleeping = Message::new_signal(
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "PrepareForSleep",
        )
        .unwrap()
        .append1(true);
        assert!(matches!(interpret(sleeping), Some(Notice::Sleep(true))));

        let waking = Message::new_signal(
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "PrepareForSleep",
        )
        .unwrap()
        .append1(false);
        assert!(matches!(interpret(waking), Some(Notice::Sleep(false))));

        let mut discovery = HashMap::new();
        discovery.insert("Discovering".to_string(), Variant(false));
        let stopped = Message::new_signal(
            "/org/bluez/hci0",
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
        )
        .unwrap()
        .append3("org.bluez.Adapter1", discovery, Vec::<String>::new());
        assert!(
            matches!(interpret(stopped), Some(Notice::DiscoveryStopped(path)) if path == "/org/bluez/hci0")
        );

        let mut changed = HashMap::new();
        changed.insert("Powered".to_string(), Variant(false));
        let powered = Message::new_signal(
            "/org/bluez/hci0",
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
        )
        .unwrap()
        .append3("org.bluez.Adapter1", changed, Vec::<String>::new());
        match interpret(powered) {
            Some(Notice::AdapterPower { path, up: false }) => {
                assert_eq!(path, "/org/bluez/hci0");
            }
            _ => panic!("expected adapter power"),
        }
    }
}
