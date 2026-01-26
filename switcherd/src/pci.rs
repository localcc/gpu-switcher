use std::{fmt::Display, path::PathBuf};

use thiserror::Error;
use tokio::{fs, io};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct PCIDeviceId {
    pub vendor_id: u16,
    pub product_id: u16,
}

#[derive(Debug)]
pub struct InvalidPCIDeviceIdError;
impl Display for InvalidPCIDeviceIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InvalidPCIDeviceIdError")
    }
}
impl std::error::Error for InvalidPCIDeviceIdError {}

impl TryFrom<&str> for PCIDeviceId {
    type Error = InvalidPCIDeviceIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let (vid, pid) = value.split_once(":").ok_or(InvalidPCIDeviceIdError)?;
        let vid = format!("{:0>4}", vid);
        let pid = format!("{:0>4}", pid);

        let mut vid = hex::decode(vid).map_err(|_| InvalidPCIDeviceIdError)?;
        vid.resize(size_of::<u16>(), 0);
        let vid = u16::from_be_bytes(
            TryInto::<[u8; size_of::<u16>()]>::try_into(vid)
                .map_err(|_| InvalidPCIDeviceIdError)?,
        );

        let mut pid = hex::decode(pid).map_err(|_| InvalidPCIDeviceIdError)?;
        pid.resize(size_of::<u16>(), 0);
        let pid = u16::from_be_bytes(
            TryInto::<[u8; size_of::<u16>()]>::try_into(pid)
                .map_err(|_| InvalidPCIDeviceIdError)?,
        );

        Ok(Self {
            vendor_id: vid,
            product_id: pid,
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct PCIBusId {
    pub domain: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl Display for PCIBusId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "{:04x}:{:02x}:{:02x}.{:01x}",
            self.domain, self.bus, self.device, self.function
        ))
    }
}

#[derive(Debug)]
pub struct InvalidPCIBusIdError;
impl Display for InvalidPCIBusIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InvalidPCIBusIdError")
    }
}
impl std::error::Error for InvalidPCIBusIdError {}

impl TryFrom<&str> for PCIBusId {
    type Error = InvalidPCIBusIdError;

    fn try_from(rest: &str) -> Result<Self, Self::Error> {
        let (domain, rest) = rest.split_once(':').ok_or(InvalidPCIBusIdError)?;
        let domain = format!("{:0>4}", domain);
        let domain = hex::decode(domain).map_err(|_| InvalidPCIBusIdError)?;
        let domain = u16::from_be_bytes(
            TryInto::<[u8; size_of::<u16>()]>::try_into(domain)
                .map_err(|_| InvalidPCIBusIdError)?,
        );

        let (bus, rest) = rest.split_once(':').ok_or(InvalidPCIBusIdError)?;
        let bus = format!("{:0>2}", bus);
        let mut bus = hex::decode(bus).map_err(|_| InvalidPCIBusIdError)?;
        bus.resize(size_of::<u8>(), 0);
        let bus = bus[0];

        let (device, function) = rest.split_once('.').unwrap_or((rest, "00"));
        let device = format!("{:0>2}", device);
        let function = format!("{:0>2}", function);

        let mut device = hex::decode(device).map_err(|_| InvalidPCIBusIdError)?;
        device.resize(size_of::<u8>(), 0);
        let device = device[0];

        let mut function = hex::decode(function).map_err(|_| InvalidPCIBusIdError)?;
        function.resize(size_of::<u8>(), 0);
        let function = function[0];

        Ok(Self {
            domain,
            bus,
            device,
            function,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PCIDevice {
    pub bus_id: PCIBusId,
    pub device_id: Option<PCIDeviceId>,
    pub slot: Option<PathBuf>,
}

impl PCIDevice {
    pub async fn find_device(bus_id: PCIBusId) -> Result<PCIDevice, PCIFindDeviceError> {
        let mut slots = tokio::fs::read_dir("/sys/bus/pci/slots")
            .await
            .map_err(PCIFindDeviceError::SlotEnumeration)?;

        while let Ok(Some(entry)) = slots.next_entry().await {
            let filename = entry.path();

            let address = std::fs::read_to_string(filename.join("address"))
                .map_err(PCIFindDeviceError::SlotAddressFetch)?;
            let address = address.trim();

            let device_bus_id = PCIBusId::try_from(address).map_err(|_| {
                PCIFindDeviceError::InvalidSlotAddress(
                    filename.to_string_lossy().to_string().into_boxed_str(),
                )
            })?;

            if device_bus_id == bus_id {
                let device_id = Self::find_udev(bus_id).ok().and_then(|e| e.device_id);

                return Ok(PCIDevice {
                    bus_id,
                    device_id,
                    slot: Some(filename),
                });
            }
        }

        Self::find_udev(bus_id)
    }

    pub async fn bind_vfio(&mut self) -> Result<(), io::Error> {
        let Some(device_id) = self.device_id else {
            return Err(io::Error::new(io::ErrorKind::NotConnected, ""));
        };

        let driver_path = "/sys/bus/pci/drivers/vfio-pci/new_id";
        let new_id = format!("{:04x} {:04x}", device_id.vendor_id, device_id.product_id);
        if let Err(e) = fs::write(driver_path, new_id.as_bytes()).await {
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(e);
            }
        }

        let has_driver = self
            .get_driver()
            .await
            .map(|e| e == "vfio-pci")
            .unwrap_or(false);

        if !has_driver {
            let driver_path = "/sys/bus/pci/drivers/vfio-pci/bind";
            let new_id = self.bus_id.to_string();
            fs::write(driver_path, new_id.as_bytes()).await?;
        }

        Ok(())
    }

    pub async fn unbind(&mut self) -> Result<(), io::Error> {
        let path = self.sysfs_path().join("driver").join("unbind");
        tokio::fs::write(path, self.bus_id.to_string().as_bytes()).await?;
        Ok(())
    }

    pub async fn remove(&mut self) -> Result<(), io::Error> {
        let path = self.sysfs_path().join("remove");
        tokio::fs::write(path, b"1").await?;
        Ok(())
    }

    pub async fn get_driver(&self) -> Result<String, io::Error> {
        let path = self.sysfs_path().join("driver");
        let path = fs::canonicalize(path).await?;
        Ok(path.file_name().unwrap().to_string_lossy().to_string())
    }

    pub fn sysfs_path(&self) -> PathBuf {
        PathBuf::from(format!("/sys/bus/pci/devices/{}", self.bus_id))
    }

    fn find_udev(bus_id: PCIBusId) -> Result<PCIDevice, PCIFindDeviceError> {
        let mut enumerator = udev::Enumerator::new().map_err(PCIFindDeviceError::Udev)?;

        enumerator
            .match_subsystem("pci")
            .map_err(PCIFindDeviceError::Subsystem)?;

        for device in enumerator
            .scan_devices()
            .map_err(PCIFindDeviceError::DeviceScan)?
        {
            let Some(slot_name) = device.property_value("PCI_SLOT_NAME") else {
                continue;
            };
            let slot_name = slot_name.to_string_lossy();

            let Some(device_id) = device.property_value("PCI_ID") else {
                continue;
            };
            let device_id = device_id.to_string_lossy();

            let Some(device_bus_id) = PCIBusId::try_from(slot_name.as_ref()).ok() else {
                continue;
            };

            let Some(device_id) = PCIDeviceId::try_from(device_id.as_ref()).ok() else {
                continue;
            };

            if bus_id == device_bus_id {
                return Ok(PCIDevice {
                    bus_id,
                    device_id: Some(device_id),
                    slot: None,
                });
            }
        }

        Err(PCIFindDeviceError::DeviceNotFound)
    }
}

#[derive(Debug, Error)]
pub enum PCIFindDeviceError {
    #[error("udev: {0:?}")]
    Udev(io::Error),
    #[error("subsystem: {0:?}")]
    Subsystem(io::Error),
    #[error("device scan: {0:?}")]
    DeviceScan(io::Error),
    #[error("slot enumeration: {0:?}")]
    SlotEnumeration(io::Error),
    #[error("slot address fetch: {0:?}")]
    SlotAddressFetch(io::Error),
    #[error("invalid slot address for slot: {0:?}")]
    InvalidSlotAddress(Box<str>),
    #[error("device not found")]
    DeviceNotFound,
}

#[derive(Debug, Error)]
pub enum PCIEnumerationError {
    #[error("udev: {0:?}")]
    Udev(io::Error),
    #[error("subsystem: {0:?}")]
    Subsystem(io::Error),
    #[error("device scan: {0:?}")]
    DeviceScan(io::Error),
}
