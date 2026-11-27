//! Whether a host USB device on Windows has its driver in WinUSB mode.
//!
//! libusb can open a device whose driver is WinUSB, libusbK or libusb-win32. QEMU's `usb-host` is
//! built on libusb and is given a device only when its driver is WinUSB, so reims-ctl checks the
//! driver service SetupAPI reports for the device before saying one can be opened. Zadig switches
//! a device's driver to WinUSB (https://zadig.akeo.ie/); the install does not do it, as it is per
//! device.

use std::mem::size_of;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation as setup;
use windows_sys::Win32::Devices::Usb::GUID_DEVINTERFACE_USB_DEVICE;

/// Whether the driver service SetupAPI names is WinUSB, and no other libusb-backed driver.
pub(crate) fn service_is_winusb(service: &str) -> bool {
    service.eq_ignore_ascii_case("WinUSB")
}

/// Whether a USB device interface path names a device with these IDs: Windows spells the IDs in
/// the symbolic link, as `vid_046d&pid_c08f`, where libusb sees vendor `0x046d`, product `0xc08f`.
pub(crate) fn path_has_ids(path: &str, vendor: u16, product: u16) -> bool {
    let ids = format!("vid_{vendor:04x}&pid_{product:04x}");
    path.to_ascii_lowercase().contains(&ids)
}

/// Whether the device with IDs `vendor` and `product` has WinUSB as its driver, by asking SetupAPI
/// for the driver service of each USB device interface present on this host. False where the
/// device is absent, the query fails, or another driver owns it.
pub fn driver_is_winusb(vendor: u16, product: u16) -> bool {
    // SAFETY: every call below passes the buffers and sizes SetupAPI asks for, and each structure
    // has its `cbSize` set before it is handed to SetupAPI. The set is closed on every path.
    unsafe {
        let set = setup::SetupDiGetClassDevsW(
            &GUID_DEVINTERFACE_USB_DEVICE,
            std::ptr::null(),
            std::ptr::null_mut(),
            setup::DIGCF_PRESENT | setup::DIGCF_DEVICEINTERFACE,
        );
        if set == -1 {
            return false;
        }
        let mut found = false;
        let mut index = 0;
        loop {
            let mut interface: setup::SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
            interface.cbSize = size_of::<setup::SP_DEVICE_INTERFACE_DATA>() as u32;
            if setup::SetupDiEnumDeviceInterfaces(
                set,
                std::ptr::null(),
                &GUID_DEVINTERFACE_USB_DEVICE,
                index,
                &mut interface,
            ) == 0
            {
                break;
            }
            index += 1;
            let mut needed = 0;
            setup::SetupDiGetDeviceInterfaceDetailW(
                set,
                &interface,
                std::ptr::null_mut(),
                0,
                &mut needed,
                std::ptr::null_mut(),
            );
            if needed == 0 {
                continue;
            }
            let mut detail = vec![0u8; needed as usize];
            let detail = detail
                .as_mut_ptr()
                .cast::<setup::SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            (*detail).cbSize = size_of::<setup::SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            let mut info: setup::SP_DEVINFO_DATA = std::mem::zeroed();
            info.cbSize = size_of::<setup::SP_DEVINFO_DATA>() as u32;
            if setup::SetupDiGetDeviceInterfaceDetailW(
                set,
                &interface,
                detail,
                needed,
                std::ptr::null_mut(),
                &mut info,
            ) == 0
            {
                continue;
            }
            let path = (*detail).DevicePath.as_ptr();
            let len = (0..).take_while(|&i| *path.add(i) != 0).count();
            let path = String::from_utf16_lossy(std::slice::from_raw_parts(path, len));
            if !path_has_ids(&path, vendor, product) {
                continue;
            }
            found = service_of(set, &info).is_some_and(|s| service_is_winusb(&s));
            if found {
                break;
            }
        }
        setup::SetupDiDestroyDeviceInfoList(set);
        found
    }
}

/// The driver service SetupAPI reports for a device, if the query succeeds.
fn service_of(set: setup::HDEVINFO, info: &setup::SP_DEVINFO_DATA) -> Option<String> {
    let mut needed = 0;
    // SAFETY: a zero-size, null buffer asks SetupAPI for the size it needs (it fails, as expected).
    unsafe {
        setup::SetupDiGetDeviceRegistryPropertyW(
            set,
            info,
            setup::SPDRP_SERVICE,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
        if needed == 0 {
            return None;
        }
        let mut bytes = vec![0u8; needed as usize];
        if setup::SetupDiGetDeviceRegistryPropertyW(
            set,
            info,
            setup::SPDRP_SERVICE,
            std::ptr::null_mut(),
            bytes.as_mut_ptr(),
            needed,
            std::ptr::null_mut(),
        ) == 0
        {
            return None;
        }
        let words: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&w| w != 0)
            .collect();
        Some(String::from_utf16_lossy(&words))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_winusb_is_accepted_as_a_driver_service() {
        assert!(service_is_winusb("WinUSB"));
        assert!(service_is_winusb("winusb"));
        assert!(!service_is_winusb("libusbK"));
        assert!(!service_is_winusb("libusb-win32"));
        assert!(!service_is_winusb("HIDClass"));
        assert!(!service_is_winusb(""));
    }

    #[test]
    fn a_usb_device_path_names_its_vendor_and_product() {
        let path = r"\\?\usb#vid_046d&pid_c08f#5&1234#a5dcbf10-6530-11d2-901f-00c04fb951ed";
        assert!(path_has_ids(path, 0x046d, 0xc08f));
        assert!(path_has_ids(&path.to_uppercase(), 0x046d, 0xc08f));
        assert!(!path_has_ids(path, 0x046d, 0xc33c));
        assert!(!path_has_ids(path, 0x1050, 0x0407));
    }
}
