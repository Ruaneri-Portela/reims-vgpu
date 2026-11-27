//! Lists the host's USB devices and opens them with libusb, on Linux, Windows and macOS.
//!
//! libusb reads a device's descriptors without opening it, so enumeration still works where the
//! user may not drive a device. A device's own string descriptors name it: its product, after its
//! maker where the product does not name the maker, or its serial where it has no product. Where
//! those strings cannot be read, the list falls back to lsusb's `usb.ids` on Linux and to the
//! vendor and product IDs elsewhere. Hubs (device class 0x09) are left out, as QEMU's `usb-host`
//! does not pass hubs through.
//!
//! QEMU's `usb-host` claims the device when the guest is given one; reims-ctl only lists and
//! chooses them. [`host_devices`] is the list the interface shows, and [`is_usable`] says whether
//! libusb can open one here, as QEMU does: on Linux with the udev rule, on Windows once the device's
//! driver is switched to WinUSB with Zadig, and on macOS with libusb's own access.

use crate::usb::{HostDevice, name_of, usb_ids_name};
use rusb::{Context, Device, DeviceDescriptor, UsbContext};

/// Where Linux keeps lsusb's names for USB vendors and products.
#[cfg(target_os = "linux")]
const USB_IDS: &str = "/usr/share/hwdata/usb.ids";

/// The USB devices plugged into the host, other than hubs, by bus and port.
pub fn host_devices() -> Vec<HostDevice> {
    let Ok(context) = Context::new() else {
        return Vec::new();
    };
    let Ok(devices) = context.devices() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for device in devices.iter() {
        let Ok(desc) = device.device_descriptor() else {
            continue;
        };
        let (vendor, product_id) = (desc.vendor_id(), desc.product_id());
        // A hub, or a device with no IDs, is not one QEMU can give a guest.
        if is_hub(desc.class_code()) || vendor == 0 || product_id == 0 {
            continue;
        }
        let bus = u32::from(device.bus_number());
        let address = u32::from(device.address());
        let ports = device.port_numbers().unwrap_or_default();
        let port = port_string(bus, &ports, address);
        let (manufacturer, product, serial) = strings(&device, &desc);
        #[cfg(target_os = "linux")]
        let ids: Option<&str> = Some(usb_ids());
        #[cfg(not(target_os = "linux"))]
        let ids: Option<&str> = None;
        let name = friendly_name(
            manufacturer.as_deref(),
            product.as_deref(),
            serial.as_deref(),
            vendor,
            product_id,
            ids,
        );
        let mut kinds = Vec::new();
        if let Ok(config) = device
            .active_config_descriptor()
            .or_else(|_| device.config_descriptor(0))
        {
            let mut codes = Vec::new();
            for interface in config.interfaces() {
                for d in interface.descriptors() {
                    codes.push((d.class_code(), d.sub_class_code(), d.protocol_code()));
                }
            }
            kinds = interface_kinds(&codes);
        }
        out.push(HostDevice {
            vendor,
            product: product_id,
            port,
            bus,
            address,
            name,
            kinds,
        });
    }
    out.sort_by_key(|a| port_order(&a.port));
    out
}

/// Whether libusb can open the device at `bus` and `address` here, as QEMU's `usb-host` does. On
/// Windows, libusb opens devices with other drivers too, so WinUSB is required as well: a device
/// whose driver Zadig has not switched is not one the guest can be given.
pub fn is_usable(bus: u32, address: u32, vendor: u16, product: u16) -> bool {
    #[cfg(windows)]
    if !crate::usb_winusb::driver_is_winusb(vendor, product) {
        return false;
    }
    #[cfg(not(windows))]
    let _ = (vendor, product);
    let Ok(context) = Context::new() else {
        return false;
    };
    let Ok(devices) = context.devices() else {
        return false;
    };
    devices.iter().any(|device| {
        u32::from(device.bus_number()) == bus
            && u32::from(device.address()) == address
            && device.open().is_ok()
    })
}

/// Whether a device class is a hub, which QEMU does not pass through.
fn is_hub(class: u8) -> bool {
    class == 0x09
}

/// A device's port as Linux names it: its bus, then its ports from the bus's root hub (`3-2.1` is
/// port 1 of the hub on port 2 of bus 3). A device with no port path is named by its address.
fn port_string(bus: u32, ports: &[u8], address: u32) -> String {
    if ports.is_empty() {
        format!("{bus}-{address}")
    } else {
        let path = ports
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(".");
        format!("{bus}-{path}")
    }
}

/// A port's place among its bus's: by bus, then port by port.
fn port_order(port: &str) -> Vec<u32> {
    port.split(['-', '.'])
        .map(|n| n.parse().unwrap_or(u32::MAX))
        .collect()
}

/// The device's own strings, read where libusb can open it; none otherwise, which leaves the name
/// to the IDs or lsusb's.
fn strings<T: UsbContext>(
    device: &Device<T>,
    desc: &DeviceDescriptor,
) -> (Option<String>, Option<String>, Option<String>) {
    let Ok(handle) = device.open() else {
        return (None, None, None);
    };
    let read = |index: Option<u8>| -> Option<String> {
        handle
            .read_string_descriptor_ascii(index?)
            .ok()
            .filter(|s| !s.is_empty())
    };
    (
        read(desc.manufacturer_string_index()),
        read(desc.product_string_index()),
        read(desc.serial_number_string_index()),
    )
}

/// A device's name from its own strings, or from lsusb's `usb.ids` where given, or from its IDs.
fn friendly_name(
    manufacturer: Option<&str>,
    product: Option<&str>,
    serial: Option<&str>,
    vendor: u16,
    product_id: u16,
    ids: Option<&str>,
) -> String {
    let manufacturer = manufacturer.filter(|s| !s.is_empty());
    let product = product.filter(|s| !s.is_empty());
    let serial = serial.filter(|s| !s.is_empty());
    if let Some(p) = product {
        return name_of(manufacturer.map(str::to_string), Some(p.to_string()))
            .unwrap_or_else(|| p.to_string());
    }
    if let Some(m) = manufacturer {
        return m.to_string();
    }
    if let Some(s) = serial {
        return s.to_string();
    }
    if let Some(ids) = ids
        && let Some(n) = usb_ids_name(ids, vendor, product_id)
    {
        return n;
    }
    format!("{vendor:04x}:{product_id:04x}")
}

/// What the device's interfaces say it is, by their class, subclass and protocol (USB-IF's class
/// codes), each kind named once.
fn interface_kinds(codes: &[(u8, u8, u8)]) -> Vec<&'static str> {
    let mut kinds = Vec::new();
    for &(class, sub, protocol) in codes {
        let kind = match (class, sub, protocol) {
            (0x03, 0x01, 0x01) => Some("keyboard"),
            (0x03, 0x01, 0x02) => Some("mouse"),
            (0x01, ..) => Some("audio"),
            (0x02, 0x02, _) => Some("serial"),
            (0x02, 0x06 | 0x0d, _) | (0xe0, 0x01, 0x03) => Some("network"),
            (0x07, ..) => Some("printer"),
            (0x08, ..) => Some("storage"),
            (0x0b, ..) => Some("smart card"),
            (0x0e, ..) => Some("camera"),
            (0xe0, 0x01, 0x01) => Some("Bluetooth"),
            _ => None,
        };
        if let Some(kind) = kind
            && !kinds.contains(&kind)
        {
            kinds.push(kind);
        }
    }
    kinds
}

/// lsusb's names, read once where they are needed: Linux's `usb.ids`.
#[cfg(target_os = "linux")]
fn usb_ids() -> &'static str {
    static IDS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    IDS.get_or_init(|| {
        String::from_utf8_lossy(&std::fs::read(USB_IDS).unwrap_or_default()).into_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::parse_usb_port;

    const USB_IDS_TEXT: &str = "8087  Intel Corp.\n\t0029  AX200 Bluetooth\n";

    #[test]
    fn hubs_are_left_out() {
        assert!(is_hub(0x09));
        for class in [0x00, 0x01, 0x02, 0x03, 0x08, 0x09 + 1, 0xef, 0xff] {
            assert!(!is_hub(class), "{class:#x}");
        }
    }

    #[test]
    fn ports_are_named_from_the_bus_and_path() {
        assert_eq!(port_string(3, &[2, 1], 4), "3-2.1");
        assert_eq!(port_string(1, &[5], 3), "1-5");
        // A device with no port path is named by its address, as Linux names one.
        assert_eq!(port_string(2, &[], 7), "2-7");
        for p in ["3-2.1", "1-5", "2-7"] {
            assert!(parse_usb_port(p).is_some(), "{p}");
        }
        assert_eq!(port_order("3-2.1"), [3, 2, 1]);
        assert_eq!(port_order("2-7"), [2, 7]);
    }

    #[test]
    fn names_come_from_the_device_s_own_strings() {
        // A product that names its maker keeps just the product; one that does not gets the maker
        // before it.
        assert_eq!(
            friendly_name(
                Some("Logitech"),
                Some("Logitech Unifying"),
                None,
                0x046d,
                0xc52b,
                None
            ),
            "Logitech Unifying"
        );
        assert_eq!(
            friendly_name(Some("FTDI"), Some("FT232R"), None, 0x0403, 0x6001, None),
            "FTDI FT232R"
        );
        // Without a product, the maker, then the serial, then lsusb's name, then the IDs.
        assert_eq!(
            friendly_name(Some("Acme"), None, Some("123"), 0x1234, 0x5678, None),
            "Acme"
        );
        assert_eq!(
            friendly_name(None, None, Some("123"), 0x1234, 0x5678, None),
            "123"
        );
        assert_eq!(
            friendly_name(None, None, None, 0x8087, 0x0029, Some(USB_IDS_TEXT)),
            "Intel Corp. AX200 Bluetooth"
        );
        assert_eq!(
            friendly_name(None, None, None, 0x1234, 0x5678, Some(USB_IDS_TEXT)),
            "1234:5678"
        );
        assert_eq!(
            friendly_name(None, None, None, 0x1234, 0x5678, None),
            "1234:5678"
        );
    }

    #[test]
    fn kinds_come_from_interface_class_codes() {
        assert_eq!(
            interface_kinds(&[(0x03, 0x01, 0x01), (0x03, 0x01, 0x02), (0x08, 0x06, 0x50)]),
            ["keyboard", "mouse", "storage"]
        );
        assert_eq!(interface_kinds(&[(0xe0, 0x01, 0x01)]), ["Bluetooth"]);
        assert_eq!(interface_kinds(&[(0xe0, 0x01, 0x03)]), ["network"]);
        assert_eq!(interface_kinds(&[(0x02, 0x02, 0x00)]), ["serial"]);
        assert!(interface_kinds(&[(0xff, 0xff, 0xff)]).is_empty());
        // A kind is named once, even where several interfaces say it.
        assert_eq!(
            interface_kinds(&[(0xe0, 0x01, 0x01), (0xe0, 0x01, 0x01)]),
            ["Bluetooth"]
        );
    }
}
