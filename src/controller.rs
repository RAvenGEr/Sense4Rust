use std::{
    cell::Cell,
    hash::Hash,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use async_hid::{AsyncHidRead, AsyncHidWrite, Device};
use thiserror::Error;
use tokio::select;
use tokio_util::sync::CancellationToken;

use crate::windows_bluetooth::disconnect_bluetooth;

const SONY_VID: u16 = 0x054C;
const DS4_PIDS: [u16; 2] = [0x05C4, 0x09CC];
const DUALSENSE_PID: u16 = 0x0CE6;

mod reports;

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("HID error: {0}")]
    Hid(#[from] async_hid::HidError),
    #[error("Windows error: {0}")]
    Win(#[from] windows::core::Error),
    #[error("Invalid HID device")]
    InvalidDevice,
    #[error("Invalid response")]
    Response(#[from] std::array::TryFromSliceError),
    #[error("Device busy")]
    Busy,
    #[error("Unsupported operation {0}")]
    Unsupported(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy)]
pub(crate) enum ControllerType {
    DualShock4,
    DualSense,
}

pub(crate) fn controller_type(device: &Device) -> Option<ControllerType> {
    if device.vendor_id != SONY_VID {
        return None;
    }
    if device.product_id == DUALSENSE_PID {
        Some(ControllerType::DualSense)
    } else if DS4_PIDS.contains(&device.product_id) {
        Some(ControllerType::DualShock4)
    } else {
        None
    }
}

#[derive(Debug, Hash, PartialEq, Eq)]
pub(crate) enum Controller {
    DualShock4(DS4Controller),
    DualSense(DualSenseController),
}

impl Controller {
    pub(crate) fn from_device(device: Device) -> Option<Controller> {
        controller_type(&device).map(|t| match t {
            ControllerType::DualShock4 => Self::DualShock4(DS4Controller::new(device)),
            ControllerType::DualSense => Self::DualSense(DualSenseController::new(device)),
        })
    }

    pub(crate) async fn from_device_async(device: Device) -> Option<Controller> {
        Self::from_device(device)
    }

    pub(crate) fn id(&self) -> &async_hid::DeviceId {
        match self {
            Controller::DualShock4(controller) => controller.id(),
            Controller::DualSense(controller) => controller.id(),
        }
    }

    pub(crate) fn set_disconnect_on_drop(&self, disconnect: bool) {
        match self {
            Controller::DualShock4(controller) => controller.set_disconnect_on_drop(disconnect),
            Controller::DualSense(controller) => controller.set_disconnect_on_drop(disconnect),
        }
    }

    pub(crate) async fn process_inputs(&self) -> Result<()> {
        match self {
            Controller::DualShock4(controller) => controller.process_inputs().await,
            Controller::DualSense(controller) => controller.process_inputs().await,
        }
    }

    async fn power_off(&mut self) -> Result<()> {
        match self {
            Controller::DualShock4(controller) => controller.power_off().await,
            Controller::DualSense(controller) => controller.power_off().await,
        }
    }

    async fn disconnect(&mut self) -> Result<()> {
        Err(Error::Unsupported(
            "Disconnect not available for USB connections",
        ))
    }
}

#[derive(Debug, PartialEq)]
enum ConnectionType {
    Usb,
    Bluetooth,
}

impl ConnectionType {
    fn report_offset(&self) -> usize {
        match self {
            ConnectionType::Usb => 0,
            ConnectionType::Bluetooth => 2,
        }
    }
}

struct ControllerDevice {
    device: async_hid::Device,
    connection: ConnectionType,
    cancel: Cell<Option<CancellationToken>>,
    disconnect_on_drop: AtomicBool,
}

impl std::fmt::Debug for ControllerDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControllerDevice")
            .field("device", &self.device)
            .field("connection", &self.connection)
            .finish()
    }
}

impl PartialEq for ControllerDevice {
    fn eq(&self, other: &Self) -> bool {
        self.device == other.device
    }
}

impl Eq for ControllerDevice {}

impl Hash for ControllerDevice {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.device.hash(state);
    }
}

impl ControllerDevice {
    fn new(device: Device) -> Self {
        let connection = if device.serial_number.is_some()
            || device.max_input_report_length.is_some_and(|len| len != 64)
        {
            ConnectionType::Bluetooth
        } else {
            ConnectionType::Usb
        };
        Self {
            device,
            cancel: Cell::new(None),
            connection,
            disconnect_on_drop: AtomicBool::new(false),
        }
    }

    fn input_busy(&self) -> bool {
        let cancel = self.cancel.replace(None);
        if cancel.is_some() {
            _ = self.cancel.replace(cancel);
            true
        } else {
            false
        }
    }

    fn bluetooth_address(&self) -> Option<u64> {
        match self.device.serial_number.as_deref() {
            Some(s) => u64::from_str_radix(s, 16).ok(),
            None => match self.connection {
                ConnectionType::Bluetooth => todo!(),
                ConnectionType::Usb => None,
            },
        }
    }

    /// Begin processing input reports
    /// WARNING: This function should not be called a second time for the same device
    async fn process_input(&self) -> Result<()> {
        let token = CancellationToken::new();
        let cloned_token = token.clone();
        _ = self.cancel.replace(Some(token));
        let is_bluetooth = self.connection == ConnectionType::Bluetooth;
        let offset = self.connection.report_offset();
        let mut reader = self.device.open_readable().await?;
        tokio::spawn(async move {
            let mut input_bytes = [0u8; 512];
            // let mut last_report = None;
            println!("Spawned input task");

            loop {
                select! {
                    _ = cloned_token.cancelled() => {
                        println!("input task cancelled");
                        break;
                    }
                    res = reader.read_input_report(&mut input_bytes) => {
                        if let Ok(len) = res {
                            // Validate CRC for Bluetooth packets
                            if is_bluetooth
                                && (input_bytes[0] != 0x11 && input_bytes[0] != 0x31 || !validate_bluetooth_crc(&input_bytes[..len])) {
                                continue;
                            }

                            // if let Ok((rep, _rem)) = InputReport::read_from_prefix(&input_bytes[offset..len]){
                            //     let this_report = rep.counter();
                            //     if let Some(last) = last_report {
                            //         if this_report == last {
                            //             println!("Duplicate report");
                            //             continue;
                            //         }
                            //         let expected = (last + 1) % 0x3F;
                            //         if this_report != expected {
                            //             println!("Expected: {expected} Received: {this_report}");
                            //         }
                            //     }
                            //     last_report = Some(this_report);
                            // }
                        }
                    }
                }
            }
        });
        Ok(())
    }

    async fn write_report(&self, report: &[u8]) -> Result<()> {
        let mut writer = self.device.open_writeable().await?;
        writer.write_output_report(report).await?;
        Ok(())
    }
}

impl Drop for ControllerDevice {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
            std::thread::sleep(Duration::from_millis(1));
        }
        if self.disconnect_on_drop.load(Ordering::Acquire)
            && let Some(addr) = self.bluetooth_address()
        {
            _ = disconnect_bluetooth(addr);
        }
    }
}

#[derive(Debug, Hash, PartialEq, Eq)]
struct DS4Controller {
    device: ControllerDevice,
}

impl DS4Controller {
    fn new(device: Device) -> Self {
        let device = ControllerDevice::new(device);
        Self { device }
    }

    #[inline]
    fn id(&self) -> &async_hid::DeviceId {
        &self.device.device.id
    }

    #[inline]
    fn input_busy(&self) -> bool {
        self.device.input_busy()
    }

    #[inline]
    fn set_disconnect_on_drop(&self, disconnect: bool) {
        self.device
            .disconnect_on_drop
            .store(disconnect, Ordering::Release);
    }

    async fn process_inputs(&self) -> Result<()> {
        if self.device.input_busy() {
            return Err(Error::Busy);
        }
        self.write_out_report().await?;
        self.device.process_input().await
    }

    async fn power_off(&self) -> Result<()> {
        if self.device.connection == ConnectionType::Usb {
            return Err(Error::Unsupported(
                "Power off not available for USB connections",
            ));
        }

        let mut report = [0u8; 78];
        report[0] = 0x11;
        report[1] = 0x80;
        report[3] = 0x08;
        self.device.write_report(&report).await
    }

    async fn write_out_report(&self) -> Result<()> {
        let mut out = [0u8; 78];
        out[0] = 0x11;
        out[1] = 0xC0;
        out[3] = 0x07;
        out[4] = 0x04;
        // TODO: Handle other controller outputs
        self.device.write_report(&out).await
    }
}

#[derive(Debug, Hash, PartialEq, Eq)]
struct DualSenseController {
    device: ControllerDevice,
}

impl DualSenseController {
    fn new(device: Device) -> Self {
        let device = ControllerDevice::new(device);
        Self { device }
    }

    #[inline]
    fn id(&self) -> &async_hid::DeviceId {
        &self.device.device.id
    }

    #[inline]
    fn input_busy(&self) -> bool {
        self.device.input_busy()
    }

    #[inline]
    fn set_disconnect_on_drop(&self, disconnect: bool) {
        self.device
            .disconnect_on_drop
            .store(disconnect, Ordering::Release);
    }

    async fn process_inputs(&self) -> Result<()> {
        if self.device.input_busy() {
            return Err(Error::Busy);
        }
        self.write_out_report().await?;
        self.device.process_input().await
    }

    async fn power_off(&self) -> Result<()> {
        if self.device.connection == ConnectionType::Usb {
            return Err(Error::Unsupported(
                "Power off not available for USB connections",
            ));
        }

        let mut report = [0u8; 78];
        report[0] = 0x31;
        report[1] = 0x02;
        report[10] = 0x02;
        append_checksum(BT_SEED, &mut report);
        self.device.write_report(&report).await
    }

    async fn write_out_report(&self) -> Result<()> {
        let mut out = [0u8; 78];
        out[0] = 0x31;
        out[1] = 0x02;
        out[2] = 0x0F;
        out[3] = 0x55;
        append_checksum(BT_SEED, &mut out);
        // TODO: Handle other controller outputs
        self.device.write_report(&out).await
    }
}

const BT_HDR: u8 = 0xA2;
const DEFAULT_SEED: u32 = 0xffffffff;
const BT_SEED: u32 = 0x8C2C830C;

#[inline]
fn calculate_checksum(init: u32, report: &[u8]) -> u32 {
    let custom_params = crc_fast::CrcParams::new(
        "CRC-32/CUSTOM",
        32,
        0x04c11db7,
        init as u64,
        true,
        0xffffffff,
        0xcbf43926,
    );
    let checksum = crc_fast::checksum_with_params(custom_params, report);
    checksum as u32
}

#[inline]
fn append_checksum(init: u32, report: &mut [u8]) {
    if report.len() < 5 {
        return;
    }
    let last = report.len() - 4;
    let crc = calculate_checksum(init, &report[..last]);
    report[last..].copy_from_slice(&crc.to_le_bytes());
}

/// Validate CRC for Bluetooth packets
/// Returns true if CRC is valid, false otherwise
/// For Bluetooth packets, the report is 78 bytes with CRC in bytes 74-77
fn validate_bluetooth_crc(report: &[u8]) -> bool {
    let cs_bytes = report[74..=77].try_into();
    if cs_bytes.is_err() {
        eprintln!("Invalid report");
        return false;
    }
    let sent_checksum = u32::from_le_bytes(cs_bytes.unwrap());
    let calculated_checksum = calculate_checksum(BT_SEED, &report[..74]);

    if calculated_checksum == sent_checksum {
        true
    } else {
        eprintln!("BT checksum fail calculated: {calculated_checksum:x} sent: {sent_checksum:x}");
        false
    }
}
