use std::mem;

use windows::Win32::Devices::Bluetooth::{
    BLUETOOTH_FIND_RADIO_PARAMS, BluetoothFindFirstRadio, BluetoothFindNextRadio,
    BluetoothFindRadioClose,
};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::IO::DeviceIoControl;

const IOCTL_BTH_DISCONNECT_DEVICE: u32 = 0x41000C;

/// Disconnect a Bluetooth device by MAC address
///
/// # Arguments
/// * `bt_address` - MAC address of the device
pub fn disconnect_bluetooth(bt_address: u64) -> windows::core::Result<bool> {
    let params = BLUETOOTH_FIND_RADIO_PARAMS {
        dwSize: mem::size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32,
    };
    let mut bt_handle: HANDLE = HANDLE::default();
    // Find first Bluetooth radio
    let search_handle = unsafe { BluetoothFindFirstRadio(&params, &mut bt_handle)? };
    let mut success;

    loop {
        let mut bytes_returned: u32 = 0;
        let mut addr = bt_address;

        // Send IOCTL to disconnect device
        let ioctl_result = unsafe {
            DeviceIoControl(
                bt_handle,
                IOCTL_BTH_DISCONNECT_DEVICE,
                Some(&mut addr as *mut _ as *mut std::ffi::c_void),
                mem::size_of::<u64>() as u32,
                None,
                0,
                Some(&mut bytes_returned),
                None,
            )
        };

        success = ioctl_result.is_ok();

        unsafe {
            // Close the handle
            let _ = windows::Win32::Foundation::CloseHandle(bt_handle);

            if success || BluetoothFindNextRadio(search_handle, &mut bt_handle).is_err() {
                break;
            }
        }
    }

    // Clean up search handle
    unsafe {
        _ = BluetoothFindRadioClose(search_handle);
    }

    Ok(success)
}
