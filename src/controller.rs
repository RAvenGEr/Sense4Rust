use std::{
    hash::Hash,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use async_hid::{AsyncHidRead, AsyncHidWrite, Device};
use thiserror::Error;
use tokio::select;
use tokio_util::sync::CancellationToken;
use zerocopy::{FromBytes, IntoBytes};

use crate::windows_bluetooth::disconnect_bluetooth;

const SONY_VID: u16 = 0x054C;
const DS4_PIDS: [u16; 2] = [0x05C4, 0x09CC];
const DUALSENSE_PID: u16 = 0x0CE6;

mod sony_reports;
use sony_reports::{Ds4BtOutput, Ds4UsbOutput, DualSenseBtOutput, DualSenseUsbOutput};

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

    pub(crate) async fn set_led(&self, r: u8, g: u8, b: u8) -> Result<()> {
        match self {
            Controller::DualShock4(controller) => controller.set_led(r, g, b).await,
            Controller::DualSense(controller) => controller.set_led(r, g, b).await,
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

struct ControllerDevice {
    device: async_hid::Device,
    connection: ConnectionType,
    cancel: CancellationToken,
    running: AtomicBool,
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
            connection,
            cancel: CancellationToken::new(),
            running: AtomicBool::new(false),
            disconnect_on_drop: AtomicBool::new(false),
        }
    }

    fn input_busy(&self) -> bool {
        self.running.load(Ordering::Acquire)
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
        let cloned_token = self.cancel.clone();
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
                        if let Ok(_len) = res {
                            // TODO: Support injection of report processing
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
        if self.input_busy() {
            self.cancel.cancel();
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
        self.set_led(0, 0, 0).await?;
        self.device.process_input().await
    }

    pub(crate) async fn set_led(&self, r: u8, g: u8, b: u8) -> Result<()> {
        match self.device.connection {
            ConnectionType::Usb => {
                let mut report = Ds4UsbOutput::new();
                // report.control_flags = 0x01 | 0x02 | 0x04; // Rumble + LED
                // report.lightbar_red = r;
                // report.lightbar_green = g;
                // report.lightbar_blue = b;
                self.device.write_report(report.as_bytes()).await
            }
            ConnectionType::Bluetooth => {
                let mut report = Ds4BtOutput::new();
                // report.control_flags = 0x01 | 0x02 | 0x04; // Rumble + LED
                // report.lightbar_red = r;
                // report.lightbar_green = g;
                // report.lightbar_blue = b;
                report.add_crc();
                self.device.write_report(report.as_bytes()).await
            }
        }
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
        self.set_led(0, 0, 0).await?;
        self.device.process_input().await
    }

    pub(crate) async fn set_led(&self, r: u8, g: u8, b: u8) -> Result<()> {
        match self.device.connection {
            ConnectionType::Usb => {
                let mut report = DualSenseUsbOutput::new();
                // report.control_flags1 = 0x01 | 0x02; // Rumble
                // report.control_flags2 = 0x04; // LED
                // report.lightbar_red = r;
                // report.lightbar_green = g;
                // report.lightbar_blue = b;
                self.device.write_report(report.as_bytes()).await
            }
            ConnectionType::Bluetooth => {
                let mut report = DualSenseBtOutput::new();
                report.payload.led_red = r;
                report.payload.led_green = g;
                report.payload.led_blue = b;
                report.add_crc();
                let bytes = report.as_bytes();
                println!("Rust DualSense BT 'Red Lightbar' Payload:");
                println!("------------------------------------------------");
                for (i, byte) in bytes.iter().enumerate() {
                    print!("{:02X} ", byte);
                    if (i + 1) % 16 == 0 {
                        println!();
                    }
                }
                println!("\n------------------------------------------------");
                self.device.write_report(bytes).await
            }
        }
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
        // append_checksum(BT_SEED, &mut report);
        self.device.write_report(&report).await
    }
}
