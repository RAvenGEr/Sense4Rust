use zerocopy::byteorder::little_endian::{I16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Calculate the CRC32 for Sony Bluetooth HID output reports.
/// Optimized using the seed 0x8C2C830C (CRC state after HID header 0xA2).
pub fn calculate_bt_crc32(report_data: &[u8]) -> u32 {
    let custom_params = crc_fast::CrcParams::new(
        "CRC-32/CUSTOM",
        32,
        0x04c11db7,
        0x8C2C830C,
        true,
        0xffffffff,
        0xcbf43926,
    );
    let checksum = crc_fast::checksum_with_params(custom_params, report_data);
    checksum as u32
}

#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Default, Debug)]
#[repr(C)]
pub struct CommonButtons {
    pub dpad_and_shapes: u8,
    pub buttons_standard: u8,
    pub buttons_system: u8,
}

#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Default, Debug)]
#[repr(C)]
pub struct ImuData {
    pub gyro_x: I16,
    pub gyro_y: I16,
    pub gyro_z: I16,
    pub accel_x: I16,
    pub accel_y: I16,
    pub accel_z: I16,
}

#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Default, Debug)]
#[repr(C)]
pub struct TouchPoint {
    pub contact: u8,
    pub x_low: u8,
    pub x_high_y_low: u8,
    pub y_high: u8,
}

// --- DualShock 4 (DS4) ---

#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Default, Debug)]
#[repr(C)]
pub struct Ds4OutputPayload {
    pub flag_0: u8,
    pub flag_1: u8,
    reserved_0: u8,
    pub motor_right: u8,
    pub motor_left: u8,
    pub led_red: u8,
    pub led_green: u8,
    pub led_blue: u8,
    pub flash_on: u8,
    pub flash_off: u8,
    reserved_1: [u8; 8],
    pub vol_speaker: u8,
    pub vol_mic: u8,
    pub vol_headphone: u8,
    pub audio_mute_flags: u8,
    reserved_2: [u8; 9],
}

impl Ds4OutputPayload {
    pub fn new() -> Self {
        Self {
            flag_0: 0x1F, // Rumble, LED, Vol enabled
            ..Default::default()
        }
    }
}

#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug)]
#[repr(C)]
pub struct Ds4BtOutput {
    pub report_id: u8,
    pub hw_control: u8,
    pub report_tag: u8,
    pub payload: Ds4OutputPayload,
    pub padding: [u8; 40],
    crc: U32,
}

impl Ds4BtOutput {
    pub fn new() -> Self {
        Self {
            report_id: 0x11,
            hw_control: 0x80,
            report_tag: 0,
            payload: Ds4OutputPayload::new(),
            padding: [0u8; 40],
            crc: 0.into(),
        }
    }

    pub fn add_crc(&mut self) {
        let b = self.as_bytes();
        self.crc = U32::from(calculate_bt_crc32(&b[..b.len() - 4]));
    }
}

// --- DualSense (PS5) ---

#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Default, Debug)]
#[repr(C)]
pub struct DualSenseOutputPayload {
    pub flag_0: u8,
    pub flag_1: u8,
    pub motor_right: u8,
    pub motor_left: u8,
    pub vol_headphone: u8,
    pub vol_speaker: u8,
    pub vol_mic: u8,
    pub audio_flags: u8,
    pub mute_button_led: u8,
    pub power_save_control: u8,
    pub trigger_l2_mode: u8,
    pub trigger_l2_params: [u8; 9],
    pub trigger_r2_mode: u8,
    pub trigger_r2_params: [u8; 9],
    reserved_0: [u8; 12],
    pub motor_flags: u8,
    pub player_leds: u8,
    pub led_red: u8,
    pub led_green: u8,
    pub led_blue: u8,
}

impl DualSenseOutputPayload {
    pub fn new() -> Self {
        Self {
            flag_0: 0x07, // Rumble, Triggers, Audio enabled
            flag_1: 0x1F, // Visuals enabled
            ..Default::default()
        }
    }
}

#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug)]
#[repr(C)]
pub struct DualSenseBtOutput {
    pub report_id: u8,
    pub tag: u8,
    pub payload: DualSenseOutputPayload,
    padding: [u8; 25],
    crc: U32,
}

impl DualSenseBtOutput {
    pub fn new() -> Self {
        Self {
            report_id: 0x31,
            tag: 0x02,
            payload: DualSenseOutputPayload::new(),
            padding: [0u8; 25],
            crc: 0.into(),
        }
    }

    pub fn add_crc(&mut self) {
        let b = self.as_bytes();
        self.crc = U32::from(calculate_bt_crc32(&b[..b.len() - 4]));
    }
}

// --- USB Wrappers ---

#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug)]
#[repr(C)]
pub struct Ds4UsbOutput {
    pub report_id: u8,
    pub payload: Ds4OutputPayload,
}

impl Ds4UsbOutput {
    pub fn new() -> Self {
        Self {
            report_id: 0x05,
            payload: Ds4OutputPayload::new(),
        }
    }
}

#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug)]
#[repr(C)]
pub struct DualSenseUsbOutput {
    pub report_id: u8,
    pub payload: DualSenseOutputPayload,
}

impl DualSenseUsbOutput {
    pub fn new() -> Self {
        Self {
            report_id: 0x02,
            payload: DualSenseOutputPayload::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerocopy::IntoBytes;

    #[test]
    fn test_report_sizes() {
        let report = Ds4UsbOutput::new();
        let bytes = report.as_bytes();
        assert_eq!(bytes.len(), 32);
        let report = Ds4BtOutput::new();
        let bytes = report.as_bytes();
        assert_eq!(bytes.len(), 78, "DS4BtOutput length is incorrect");
        let report = DualSenseUsbOutput::new();
        let bytes = report.as_bytes();
        assert_eq!(bytes.len(), 48);
        let report = DualSenseBtOutput::new();
        let bytes = report.as_bytes();
        assert_eq!(bytes.len(), 78, "DualSenseBtOutput length is incorrect");
    }
}