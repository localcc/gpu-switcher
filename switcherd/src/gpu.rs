use std::path::{Path, PathBuf};

use color_eyre::{eyre::Context, Section};
use thiserror::Error;
use tokio::io;
use tracing::warn;

use crate::{PCIBusId, PCIDevice};

#[derive(Debug, Clone)]
pub struct GpuDevice {
    pub pci: PCIDevice,
    pub audio_pci: Option<PCIDevice>,
    pub cards: Vec<u8>,
    pub render_devices: Vec<u8>,
    pub gpu_slot: Option<String>,
}

impl GpuDevice {
    pub async fn new(device: PCIDevice) -> Result<Self, GpuDeviceCreationError> {
        let audio_pci = PCIDevice::find_device(PCIBusId {
            function: 1,
            ..device.bus_id
        })
        .await
        .ok();

        let sysfs = device.sysfs_path();

        let drm_path = sysfs.join("drm");
        let mut cards = Vec::new();
        let mut render_devices = Vec::new();

        let mut slot = None;

        if let Ok(mut dir_entries) = tokio::fs::read_dir(&drm_path).await {
            while let Ok(Some(entry)) = dir_entries.next_entry().await {
                let filename = entry.file_name();
                let filename = filename.to_string_lossy();

                if let Some(card_number) = filename.strip_prefix("card") {
                    let card_number = card_number.parse::<u8>().map_err(|_| {
                        GpuDeviceCreationError::InvalidCardNumber(
                            filename.to_string().into_boxed_str(),
                        )
                    })?;

                    cards.push(card_number);
                };

                if let Some(render_device) = filename.strip_prefix("renderD") {
                    let render_device_number = render_device.parse::<u8>().map_err(|_| {
                        GpuDeviceCreationError::InvalidRenderDeviceNumber(
                            filename.to_string().into_boxed_str(),
                        )
                    })?;

                    render_devices.push(render_device_number);
                };
            }
        }

        let mut slots = tokio::fs::read_dir("/sys/bus/pci/slots")
            .await
            .map_err(GpuDeviceCreationError::SlotEnumeration)?;

        while let Ok(Some(entry)) = slots.next_entry().await {
            let filename = entry.path();
            let filename = Path::new(&filename);

            let address = std::fs::read_to_string(filename.join("address"))
                .map_err(GpuDeviceCreationError::SlotAddressFetch)?;
            let address = address.trim();

            let bus_id = PCIBusId::try_from(address).map_err(|_| {
                GpuDeviceCreationError::InvalidSlotAddress(
                    filename.to_string_lossy().to_string().into_boxed_str(),
                )
            })?;

            if bus_id == device.bus_id {
                slot = Some(filename.to_string_lossy().to_string());
                break;
            }
        }

        Ok(Self {
            pci: device,
            audio_pci,
            cards,
            render_devices,
            gpu_slot: slot,
        })
    }

    pub async fn send_detach(&mut self) -> Result<(), io::Error> {
        let drm_path = self.drm_path();
        for card in &self.cards {
            let path = drm_path.join(format!("card{}", card)).join("uevent");
            if !path.exists() {
                warn!(
                    "cannot detach card {}, uevent path doesn't exist",
                    path.display()
                );
                continue;
            }

            tokio::fs::write(path, b"remove").await?;
        }

        Ok(())
    }

    pub async fn bind_vfio(&mut self) -> Result<(), io::Error> {
        self.pci.bind_vfio().await?;
        if let Some(audio) = self.audio_pci.as_mut() {
            audio.bind_vfio().await?;
        }
        Ok(())
    }

    pub async fn unbind(&mut self) -> Result<(), color_eyre::Report> {
        let mut gpu_unbind = self.pci.unbind().await.wrap_err("gpu unbind");
        if let Some(audio) = self.audio_pci.as_mut() {
            if let Err(e) = audio.unbind().await {
                gpu_unbind = gpu_unbind.with_error(|| e);
            }
        }

        gpu_unbind
    }

    pub fn drm_path(&self) -> PathBuf {
        self.pci.sysfs_path().join("drm")
    }
}

#[derive(Debug, Error)]
pub enum GpuDeviceCreationError {
    #[error("invalid card number for device: {0:?}")]
    InvalidCardNumber(Box<str>),
    #[error("invalid render device number for device: {0:?}")]
    InvalidRenderDeviceNumber(Box<str>),
    #[error("slot enumeration: {0:?}")]
    SlotEnumeration(io::Error),
    #[error("slot address fetch: {0:?}")]
    SlotAddressFetch(io::Error),
    #[error("invalid slot address for slot: {0:?}")]
    InvalidSlotAddress(Box<str>),
}
