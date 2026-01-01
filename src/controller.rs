use std::{cell::Cell, hash::Hash, time::Duration};

use async_hid::{AsyncHidRead, AsyncHidWrite, Device};
use crc::{CRC_32_ISO_HDLC, Crc};
use thiserror::Error;
use tokio::select;
use tokio_util::sync::CancellationToken;
use zerocopy::FromBytes;

use crate::windows_bluetooth::disconnect_bluetooth;

const SONY_VID: u16 = 0x054C;
const DS4_PIDS: [u16; 2] = [0x05C4, 0x09CC];
const DUALSENSE_PID: u16 = 0x0CE6;

const OUT_REPORT_LEN: usize = 78;

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
    DualShock4(ControllerDevice),
    DualSense(ControllerDevice),
}

impl Controller {
    pub(crate) fn from_device(device: Device) -> Option<Controller> {
        controller_type(&device).map(|t| match t {
            ControllerType::DualShock4 => Self::DualShock4(ControllerDevice::new(device)),
            ControllerType::DualSense => Self::DualSense(ControllerDevice::new(device)),
        })
    }

    pub(crate) async fn from_device_async(device: Device) -> Option<Controller> {
        Self::from_device(device)
    }

    pub(crate) fn id(&self) -> &async_hid::DeviceId {
        &self.device().device.id
    }

    pub(crate) fn set_disconnect_on_drop(&mut self, disconnect: bool) {
        self.device_mut().disconnect_on_drop = disconnect;
    }

    pub(crate) async fn process_inputs(&self) -> Result<()> {
        let dev = self.device();
        dev.process_input().await
    }

    async fn power_off(&mut self) -> Result<()> {
        let device = self.device_mut();
        if device.connection == ConnectionType::Usb {
            return Err(Error::Unsupported(
                "Power off not available for USB connections",
            ));
        }

        let mut report = empty_out_report();
        match self {
            Controller::DualShock4(device) => {
                report[0] = 0x11;
                report[1] = 0x80;
                report[3] = 0x08;
                device.write_report(&mut report).await
            }
            Controller::DualSense(device) => {
                report[0] = 0x31;
                report[1] = 0x02;
                report[10] = 0x02;
                device.write_report(&mut report).await
            }
        }
    }

    async fn disconnect(&mut self) -> Result<()> {
        Err(Error::Unsupported(
            "Disconnect not available for USB connections",
        ))
    }

    #[inline]
    fn device_mut(&mut self) -> &mut ControllerDevice {
        match self {
            Controller::DualShock4(controller_device) => controller_device,
            Controller::DualSense(controller_device) => controller_device,
        }
    }

    #[inline]
    fn device(&self) -> &ControllerDevice {
        match &self {
            Controller::DualShock4(controller_device) => controller_device,
            Controller::DualSense(controller_device) => controller_device,
        }
    }
}

type OutReport = [u8; OUT_REPORT_LEN];

#[inline(always)]
fn empty_out_report() -> OutReport {
    [0u8; OUT_REPORT_LEN]
}

#[derive(Debug, PartialEq)]
enum ConnectionType {
    Usb,
    Bluetooth,
}

struct ControllerDevice {
    device: async_hid::Device,
    connection: ConnectionType,
    cancel: Cell<Option<CancellationToken>>,
    disconnect_on_drop: bool,
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
            disconnect_on_drop: false,
        }
    }

    fn bluetooth_address(&self) -> Option<u64> {
        match self.device.serial_number.as_deref() {
            Some(s) => u64::from_str_radix(s, 16).ok(),
            None => todo!(),
        }
    }

    async fn process_input(&self) -> Result<()> {
        let cloned_token;
        {
            let token = CancellationToken::new();
            cloned_token = token.clone();
            let cancel = self.cancel.replace(Some(token));
            if cancel.is_some() {
                _ = self.cancel.replace(cancel);
                return Err(Error::Busy);
            }
        }
        let mut reader = self.device.open_readable().await?;
        tokio::spawn(async move {
            let mut input_bytes = [0u8; 512];
            println!("Spawned input task");

            loop {
                select! {
                    _ = cloned_token.cancelled() => {
                        println!("Cancelled");
                        break;
                    }
                    res = reader.read_input_report(&mut input_bytes) => {
                        if let Ok(len) = res {
                            // TODO: validate CRC??
                            if let Ok((rep, _rem)) = InputReport::read_from_prefix(&input_bytes[..len]){
                                println!("Have report: {}", rep.counter());
                            }
                        }
                    }
                }
            }
        });
        Ok(())
    }

    async fn write_report(&self, report: &mut OutReport) -> Result<()> {
        let mut writer = self.device.open_writeable().await?;
        Self::write_report_writer(&mut writer, report).await
    }

    async fn write_report_writer<T: AsyncHidWrite>(
        writer: &mut T,
        report: &mut OutReport,
    ) -> Result<()> {
        add_ds_checksum(report);
        println!("Sending report");
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
        if self.disconnect_on_drop
            && let Some(addr) = self.bluetooth_address()
        {
            _ = disconnect_bluetooth(addr);
        }
    }
}

const CASTAGNOLI: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);
const BT_HDR: u8 = 0xA2;

fn add_ds_checksum(report: &mut OutReport) {
    // CRC calculation: Seed with 0xA2, then the report up to the CRC offset
    let mut digest = CASTAGNOLI.digest();
    digest.update(&[BT_HDR]);
    digest.update(&report[0..74]);
    let checksum = digest.finalize();

    report[74..78].copy_from_slice(&checksum.to_le_bytes());
}

pub struct DPadState {
    pub up: bool,
    pub right: bool,
    pub down: bool,
    pub left: bool,
}

impl DPadState {
    fn new(up: bool, right: bool, down: bool, left: bool) -> Self {
        Self {
            up,
            right,
            down,
            left,
        }
    }
}

#[derive(FromBytes, Debug)]
#[repr(C, packed)]
pub struct InputReport {
    report_id: u8,
    left_stick_x: u8,
    left_stick_y: u8,
    right_stick_x: u8,
    right_stick_y: u8,
    buttons_1: u8,
    buttons_2: u8,
    buttons_3: u8,
}

impl InputReport {
    #[inline]
    pub fn counter(&self) -> u8 {
        self.buttons_3 >> 2
    }

    pub fn dpad_state(&self) -> DPadState {
        let val = self.buttons_1 & 0x0F; // Mask out the upper buttons
        match val {
            0 => DPadState::new(true, false, false, false), // North (Up)
            1 => DPadState::new(true, true, false, false),  // North-East
            2 => DPadState::new(false, true, false, false), // East (Right)
            3 => DPadState::new(false, true, true, false),  // South-East
            4 => DPadState::new(false, false, true, false), // South (Down)
            5 => DPadState::new(false, false, true, true),  // South-West
            6 => DPadState::new(false, false, false, true), // West (Left)
            7 => DPadState::new(true, false, false, true),  // North-West
            _ => DPadState::new(false, false, false, false), // Released
        }
    }

    // --- Shapes ---

    #[inline]
    pub fn triangle(&self) -> bool {
        (self.buttons_1 & 0x80) != 0
    }
    #[inline]
    pub fn circle(&self) -> bool {
        (self.buttons_1 & 0x40) != 0
    }
    #[inline]
    pub fn cross(&self) -> bool {
        (self.buttons_1 & 0x20) != 0
    }
    #[inline]
    pub fn square(&self) -> bool {
        (self.buttons_1 & 0x10) != 0
    }

    // --- Shoulders & Sticks ---

    #[inline]
    pub fn l1(&self) -> bool {
        (self.buttons_2 & 0x01) != 0
    }
    #[inline]
    pub fn r1(&self) -> bool {
        (self.buttons_2 & 0x02) != 0
    }
    #[inline]
    pub fn l2(&self) -> bool {
        (self.buttons_2 & 0x04) != 0
    }
    #[inline]
    pub fn r2(&self) -> bool {
        (self.buttons_2 & 0x08) != 0
    }
    #[inline]
    pub fn l3(&self) -> bool {
        (self.buttons_2 & 0x40) != 0
    }
    #[inline]
    pub fn r3(&self) -> bool {
        (self.buttons_2 & 0x80) != 0
    }

    // --- Center Buttons ---

    #[inline]
    pub fn share(&self) -> bool {
        (self.buttons_2 & 0x10) != 0
    }
    #[inline]
    pub fn options(&self) -> bool {
        (self.buttons_2 & 0x20) != 0
    }
    #[inline]
    pub fn ps_home(&self) -> bool {
        (self.buttons_3 & 0x01) != 0
    }

    // --- Touchpad ---

    #[inline]
    pub fn touchpad_click(&self) -> bool {
        (self.buttons_3 & 0x02) != 0
    }
}
