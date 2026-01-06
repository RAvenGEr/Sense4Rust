use std::{collections::HashSet, time::Duration};

use async_hid::{DeviceEvent, HidBackend};
use futures_util::StreamExt;
use tokio::{select, signal};

use crate::controller::Controller;

mod controller;
mod windows_bluetooth;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Searching for controllers...");
    let backend = HidBackend::default();

    let mut watcher = backend.watch()?;
    let mut device_set = backend
        .enumerate()
        .await?
        .filter_map(Controller::from_device_async)
        .collect::<HashSet<_>>()
        .await;
    for dev in device_set.iter() {
        println!("Controller: {dev:?}");
        dev.set_disconnect_on_drop(true);
    }

    println!("Number of connected devices: {}", device_set.len());
    loop {
        select! {
            _ = signal::ctrl_c() => {
                println!("Cancelled");
                break;
            }
            event = watcher.next() => {
                if let Some(event) = event {
                    match event {
                        DeviceEvent::Connected(id) => {
                            let new_dev = backend
                                .query_devices(&id)
                                .await?
                                .filter_map(|dev| {
                                    let mut controller =
                                    Controller::from_device(dev);
                                    if let Some(cont) = controller.as_mut() {
                                        cont.set_disconnect_on_drop(true);
                                    }
                                    controller
                                    }
                                )
                                .collect::<HashSet<_>>();
                            for controller in new_dev.iter() {
                                println!("Controller: {controller:?}");

                                 tokio::time::sleep(Duration::from_millis(100)).await;
                                 if let Err(res) = controller.process_inputs().await {
                                     eprintln!("Error starting processing: {res}");
                                 }
                            }
                            device_set.extend(new_dev);
                        }
                        DeviceEvent::Disconnected(id) => device_set.retain(|device| *device.id() != id),
                    }
                println!("Number of connected devices: {}", device_set.len());
                }
            }
        }
    }
    Ok(())
}
